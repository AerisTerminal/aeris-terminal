//! Bounded provider-neutral market stream command and event boundary.

use crate::generation::MarketGeneration;
use crate::provenance::ProvenancedMarketBar;
use crate::replay_snapshot::ReplayStreamUpdate;
use axiusflow_protocols::Provenanced;
use core::fmt;
use std::error::Error;

/// One transport-neutral publication with its validated immutable model generation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarketStreamPublication {
    subscription_id: String,
    update: ReplayStreamUpdate,
    generation: MarketGeneration<ProvenancedMarketBar>,
    predecessor_generation: Option<u64>,
}

impl MarketStreamPublication {
    /// Creates a publication only when the update and generation describe the same state.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty subscription or contradictory update/generation evidence.
    pub fn try_new(
        subscription_id: String,
        update: ReplayStreamUpdate,
        generation: MarketGeneration<ProvenancedMarketBar>,
    ) -> Result<Self, MarketStreamPublicationError> {
        if subscription_id.is_empty() {
            return Err(MarketStreamPublicationError::EmptySubscriptionId);
        }
        let predecessor_generation = validate_stream_publication(&update, &generation)?;
        Ok(Self {
            subscription_id,
            update,
            generation,
            predecessor_generation,
        })
    }

    #[must_use]
    pub fn subscription_id(&self) -> &str {
        &self.subscription_id
    }

    #[must_use]
    pub const fn update(&self) -> &ReplayStreamUpdate {
        &self.update
    }

    #[must_use]
    pub const fn generation(&self) -> &MarketGeneration<ProvenancedMarketBar> {
        &self.generation
    }

    #[must_use]
    pub const fn predecessor_generation(&self) -> Option<u64> {
        self.predecessor_generation
    }

    #[must_use]
    pub fn into_parts(
        self,
    ) -> (
        String,
        ReplayStreamUpdate,
        MarketGeneration<ProvenancedMarketBar>,
        Option<u64>,
    ) {
        (
            self.subscription_id,
            self.update,
            self.generation,
            self.predecessor_generation,
        )
    }
}

/// Contradictory evidence at the neutral stream publication boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarketStreamPublicationError {
    EmptySubscriptionId,
    UpdateGenerationMismatch(&'static str),
}

impl fmt::Display for MarketStreamPublicationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "invalid market stream publication: {self:?}")
    }
}

impl Error for MarketStreamPublicationError {}

fn validate_stream_publication(
    update: &ReplayStreamUpdate,
    generation: &MarketGeneration<ProvenancedMarketBar>,
) -> Result<Option<u64>, MarketStreamPublicationError> {
    let mismatch = |field| MarketStreamPublicationError::UpdateGenerationMismatch(field);
    let (generation_first, generation_last) = generation.sequence_range();
    let predecessor_generation = match update {
        ReplayStreamUpdate::Snapshot(snapshot) => {
            let evidence = snapshot.evidence();
            if generation.partition_id() != evidence.partition_id {
                return Err(mismatch("snapshot partition"));
            }
            if generation.ownership_epoch() != evidence.ownership_epoch {
                return Err(mismatch("snapshot ownership epoch"));
            }
            if generation.generation() != evidence.generation {
                return Err(mismatch("snapshot generation"));
            }
            if generation_last != evidence.last_sequence {
                return Err(mismatch("snapshot last sequence"));
            }
            let retained_start = snapshot
                .bars()
                .len()
                .checked_sub(generation.items().len())
                .ok_or_else(|| mismatch("snapshot retained item count"))?;
            if generation.items() != &snapshot.bars()[retained_start..] {
                return Err(mismatch("snapshot retained items"));
            }
            None
        }
        ReplayStreamUpdate::Delta(delta) => {
            let provenance = delta.item().provenance();
            if generation.partition_id() != provenance.partition_id {
                return Err(mismatch("delta partition"));
            }
            if generation.ownership_epoch() != provenance.ownership_epoch {
                return Err(mismatch("delta ownership epoch"));
            }
            if generation_last != delta.sequence() {
                return Err(mismatch("delta last sequence"));
            }
            if generation.items().last() != Some(delta.item()) {
                return Err(mismatch("delta latest item"));
            }
            Some(
                generation
                    .generation()
                    .checked_sub(1)
                    .filter(|predecessor| *predecessor != 0)
                    .ok_or_else(|| mismatch("delta predecessor generation"))?,
            )
        }
    };
    let first_item_sequence = generation
        .items()
        .first()
        .map(Provenanced::provenance)
        .map(|provenance| provenance.source_sequence);
    let last_item_sequence = generation
        .items()
        .last()
        .map(Provenanced::provenance)
        .map(|provenance| provenance.source_sequence);
    if first_item_sequence != Some(generation_first) || last_item_sequence != Some(generation_last)
    {
        return Err(mismatch("generation item sequence range"));
    }
    Ok(predecessor_generation)
}
