//! Bounded native study dependency runtime.
//!
//! This module deliberately stops at the market-runtime ownership boundary. Studies declare
//! dependencies on already-resolved canonical market series or earlier study outputs; provider
//! sessions, history, live handoff, and canonical market state remain owned by `MarketEngine` and
//! `market_service`. The runtime produces deterministic recalculation plans and shared upstream
//! stream requirements without creating provider work itself.

use aeris_market_data::BarSeriesKey;
use aeris_market_engine::{
    ConsumerId, MarketDataLeaseId, MarketEngine, MarketStream, StreamRequirements,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    num::NonZeroU64,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{Arc, OnceLock},
};

mod alerts;
mod context;
mod error;
mod execution;
mod market_views;
mod output;
mod scene;
mod settings;
mod state;

use alerts::StudyAlertLedger;
pub use alerts::{
    MAXIMUM_STUDY_ALERT_MESSAGE_BYTES, MAXIMUM_STUDY_ALERTS_PER_WAVE, StudyAlertEvent,
    StudyAlertFrequency,
};
pub use context::{
    StudyChartContext, StudyExecutionMeta, StudyMtfConfirmation, StudyMtfGapPolicy,
    StudyRequestContext, align_higher_timeframe_bar, intrabar_range,
};
pub use error::StudyRuntimeError;
pub(crate) use market_views::StudyNonBarChange;
pub use market_views::{
    MAXIMUM_STUDY_EVENT_TIMELINE_POINTS, StudyBarField, StudyBarFieldSeries, StudyDepthView,
    StudyEventCoalescePolicy, StudyEventKind, StudyEventPoint, StudyEventTimeline, StudyInstanceId,
    StudyLiveMarketData, StudyMarketInput, StudyMarketSeries, StudyOutputId, StudyQuoteView,
    StudyTradeSample, StudyTradeWindow,
};
pub use output::{
    MAXIMUM_STUDY_FILLS, MAXIMUM_STUDY_LEVELS, StudyDefinition, StudyDependency,
    StudyExecutionContext, StudyExecutionInputs, StudyFillSpec, StudyInputSeries,
    StudyInvalidationPolicy, StudyLevelSpec, StudyLineStyle, StudyOutputBuffer, StudyOutputSeries,
    StudyOutputSpec, StudyPaneTarget, StudyPlotKind, StudyPointStyle, StudyRuntimeConfig,
    StudyScaleTarget, StudyStyleOverlay, StudyThresholdRegion,
};
use output::{ResolvedStudyInput, StudyOutputTimeline};
pub use scene::{
    MAXIMUM_STUDY_POLYLINE_POINTS, MAXIMUM_STUDY_SCENE_OBJECTS, MAXIMUM_STUDY_SCENE_TABLES,
    MAXIMUM_STUDY_SCENE_TEXT_BYTES, MAXIMUM_STUDY_TABLE_CELLS, StudyBoxObject, StudyLabelObject,
    StudyLabelStyle, StudyLineExtend, StudyLineFillObject, StudyLineObject, StudyPoint,
    StudyPolylineObject, StudySceneDelta, StudySceneObject, StudySceneObjectId, StudySceneState,
    StudyTableCell, StudyTableObject, StudyTablePosition, StudyTextAlign,
};
pub use settings::{
    StudyColor, StudyDecimal, StudySession, StudySettingChoiceOption, StudySettingCondition,
    StudySettingControl, StudySettingPresentation, StudySettingSpec, StudySettingValue,
    StudySettings, StudySource, StudySymbolRef, StudyTimeframe, resolve_market_input,
};
use settings::{validate_setting_specs, validate_settings};
pub(crate) use state::StudyExecutionBatch;
pub use state::{
    NativeStudyCalculate, NativeStudyProgram, NativeStudyRegistration, NativeStudyState,
    NativeStudyStateFactory, StudyCalculation, StudyDirtyRange, StudyMarketLeaseChange,
    StudyMarketLeaseChangeKind,
};

/// Maximum UTF-8 bytes accepted in one stable study identifier.
pub const MAXIMUM_STUDY_IDENTIFIER_BYTES: usize = 128;
/// Maximum typed settings declared by one study.
pub const MAXIMUM_STUDY_SETTINGS: usize = 64;
/// Maximum static dependencies accepted for one production study instance.
pub const MAXIMUM_STUDY_DEPENDENCIES_PER_INSTANCE: usize = 16;
/// Maximum scalar outputs accepted for one production study instance.
pub const MAXIMUM_STUDY_OUTPUTS_PER_INSTANCE: usize = 16;
/// Maximum UTF-8 bytes accepted in one stable setting identifier.
pub const MAXIMUM_STUDY_SETTING_IDENTIFIER_BYTES: usize = 64;
/// Maximum UTF-8 bytes retained in one textual setting value.
pub const MAXIMUM_STUDY_SETTING_TEXT_BYTES: usize = 512;
/// Maximum stable choice options declared by one setting.
pub const MAXIMUM_STUDY_SETTING_CHOICE_OPTIONS: usize = 64;
/// Maximum decimal scale accepted by the typed settings surface.
pub const MAXIMUM_STUDY_SETTING_DECIMAL_SCALE: u8 = 18;
const MAXIMUM_STUDY_EXECUTION_ERROR_BYTES: usize = 256;

#[derive(Clone)]
struct StudyNode {
    owner: Option<ConsumerId>,
    definition: StudyDefinition,
    settings: StudySettings,
    program: Option<NativeStudyProgram>,
    state: Option<NativeStudyState>,
    state_bytes: usize,
    intrabar_state: Option<NativeStudyState>,
    intrabar_state_bytes: usize,
}

pub(crate) struct StudySubtreeCheckpoint {
    nodes: BTreeMap<StudyInstanceId, StudyNode>,
    outputs: BTreeMap<StudyOutputId, StudyOutputSeries>,
    style_overlays: BTreeMap<StudyInstanceId, StudyStyleOverlay>,
    scenes: BTreeMap<StudyInstanceId, StudySceneState>,
    alert_ledgers: BTreeMap<StudyInstanceId, StudyAlertLedger>,
    output_points: usize,
    state_bytes: usize,
}

struct NativeStudyCalculation<'a> {
    study_id: StudyInstanceId,
    settings: &'a StudySettings,
    program: NativeStudyProgram,
    output_count: usize,
    inputs: &'a [ResolvedStudyInput],
    live_inputs: &'a [Option<StudyLiveMarketData<'a>>],
    timeline: &'a StudyOutputTimeline,
    dirty: StudyDirtyRange,
    meta: StudyExecutionMeta,
    chart_context: StudyChartContext,
}

struct CalculatedStudyOutputs {
    outputs: Vec<StudyOutputBuffer>,
    style_overlay: StudyStyleOverlay,
    scene: StudySceneState,
    alerts: Vec<StudyAlertEvent>,
    #[cfg(test)]
    preparation_work_rows: usize,
}

struct PreparedStudyStates {
    states: BTreeMap<
        StudyInstanceId,
        (
            Option<NativeStudyState>,
            usize,
            Option<NativeStudyState>,
            usize,
        ),
    >,
    total_bytes: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct StudyMarketLease {
    lease_id: MarketDataLeaseId,
    streams: StreamRequirements,
}

/// Bounded dependency owner for native studies.
///
/// Registration order is dependency order: a study may reference only outputs that already exist.
/// That rule makes cycles impossible without maintaining a second graph authority.
pub struct StudyRuntime {
    executor: OnceLock<Result<execution::Executor, String>>,
    config: StudyRuntimeConfig,
    studies: BTreeMap<StudyInstanceId, StudyNode>,
    market_dependents: BTreeMap<BarSeriesKey, BTreeSet<StudyInstanceId>>,
    output_dependents: BTreeMap<StudyOutputId, BTreeSet<StudyInstanceId>>,
    market_leases: BTreeMap<BarSeriesKey, StudyMarketLease>,
    chart_contexts: BTreeMap<ConsumerId, StudyChartContext>,
    outputs: BTreeMap<StudyOutputId, StudyOutputSeries>,
    style_overlays: BTreeMap<StudyInstanceId, StudyStyleOverlay>,
    scenes: BTreeMap<StudyInstanceId, StudySceneState>,
    scene_deltas: BTreeMap<StudyInstanceId, Vec<StudySceneDelta>>,
    alert_ledgers: BTreeMap<StudyInstanceId, StudyAlertLedger>,
    pending_alerts: BTreeMap<StudyInstanceId, Vec<StudyAlertEvent>>,
    output_points: usize,
    state_bytes: usize,
    next_id: u64,
    next_output_generation: u64,
    #[cfg(test)]
    last_output_preparation_work_rows: usize,
}

fn validate_study_outputs(outputs: &[StudyOutputSpec]) -> Result<(), StudyRuntimeError> {
    let mut identifiers = BTreeSet::new();
    for output in outputs {
        let identifier = output.identifier.trim();
        if identifier.is_empty() {
            return Err(StudyRuntimeError::InvalidOutputIdentifier);
        }
        if output.title.trim().is_empty()
            || output
                .legend_label
                .as_ref()
                .is_some_and(|label| label.trim().is_empty())
        {
            return Err(StudyRuntimeError::InvalidOutputTitle);
        }
        if identifier.len() > MAXIMUM_STUDY_IDENTIFIER_BYTES
            || output.title.len() > MAXIMUM_STUDY_IDENTIFIER_BYTES
            || output
                .legend_label
                .as_ref()
                .is_some_and(|label| label.len() > MAXIMUM_STUDY_IDENTIFIER_BYTES)
        {
            return Err(StudyRuntimeError::OutputMetadataTooLong {
                maximum: MAXIMUM_STUDY_IDENTIFIER_BYTES,
            });
        }
        if !identifiers.insert(identifier) {
            return Err(StudyRuntimeError::DuplicateOutputIdentifier);
        }
        if let Some(region) = output.threshold_region {
            let scale = region.lower.scale.max(region.upper.scale);
            if scale > MAXIMUM_STUDY_SETTING_DECIMAL_SCALE {
                return Err(StudyRuntimeError::InvalidOutputPresentation);
            }
            let lower = region
                .lower
                .scaled_mantissa(scale)
                .ok_or(StudyRuntimeError::InvalidOutputPresentation)?;
            let upper = region
                .upper
                .scaled_mantissa(scale)
                .ok_or(StudyRuntimeError::InvalidOutputPresentation)?;
            if lower >= upper || !matches!(output.plot, StudyPlotKind::Line | StudyPlotKind::Area) {
                return Err(StudyRuntimeError::InvalidOutputPresentation);
            }
        }
        if output.point_style == StudyPointStyle::MomentumHistogram
            && output.plot != StudyPlotKind::Histogram
        {
            return Err(StudyRuntimeError::InvalidOutputPresentation);
        }
    }
    Ok(())
}

impl StudyRuntime {
    /// Creates an empty bounded runtime.
    #[must_use]
    pub fn new(config: StudyRuntimeConfig) -> Self {
        Self {
            executor: OnceLock::new(),
            config,
            studies: BTreeMap::new(),
            market_dependents: BTreeMap::new(),
            output_dependents: BTreeMap::new(),
            market_leases: BTreeMap::new(),
            chart_contexts: BTreeMap::new(),
            outputs: BTreeMap::new(),
            style_overlays: BTreeMap::new(),
            scenes: BTreeMap::new(),
            scene_deltas: BTreeMap::new(),
            alert_ledgers: BTreeMap::new(),
            pending_alerts: BTreeMap::new(),
            output_points: 0,
            state_bytes: 0,
            next_id: 1,
            next_output_generation: 1,
            #[cfg(test)]
            last_output_preparation_work_rows: 0,
        }
    }

    /// Returns the last committed per-bar style overlay, levels, and fills for one study.
    #[must_use]
    pub fn style_overlay(&self, study_id: StudyInstanceId) -> Option<&StudyStyleOverlay> {
        self.style_overlays.get(&study_id)
    }

    /// Resolves a study's committed style overlay indices against its committed output timeline.
    #[must_use]
    pub fn resolved_style_overlay(
        &self,
        study_id: StudyInstanceId,
    ) -> Option<output::ResolvedStudyStyleOverlay> {
        let overlay = self.style_overlays.get(&study_id)?;
        if overlay.bar_colors.is_empty()
            && overlay.background_colors.is_empty()
            && overlay.levels.is_empty()
            && overlay.fills.is_empty()
        {
            return None;
        }
        let series = self.outputs.get(&StudyOutputId {
            study_id,
            output_index: 0,
        });
        let bar_colors = series.map_or_else(Vec::new, |series| {
            overlay
                .bar_colors
                .iter()
                .filter_map(|(&index, &color)| {
                    series
                        .timestamp_unix_nanos(index)
                        .map(|timestamp| (timestamp, color))
                })
                .collect()
        });
        let background_colors = series.map_or_else(Vec::new, |series| {
            overlay
                .background_colors
                .iter()
                .filter_map(|(&index, &color)| {
                    series
                        .timestamp_unix_nanos(index)
                        .map(|timestamp| (timestamp, color))
                })
                .collect()
        });
        Some(output::ResolvedStudyStyleOverlay {
            bar_colors,
            background_colors,
            levels: overlay.levels.clone(),
            fills: overlay.fills.clone(),
        })
    }

    /// Returns the last committed semantic drawing/table scene for one study.
    #[must_use]
    pub fn scene(&self, study_id: StudyInstanceId) -> Option<&StudySceneState> {
        self.scenes.get(&study_id)
    }

    /// Drains the compact `Create`/`Update`/`Delete` scene deltas emitted since the last drain.
    pub fn take_scene_deltas(&mut self, study_id: StudyInstanceId) -> Vec<StudySceneDelta> {
        self.scene_deltas.remove(&study_id).unwrap_or_default()
    }

    /// Drains the committed, deduplicated alert events emitted by one study for host delivery.
    pub fn take_triggered_alerts(&mut self, study_id: StudyInstanceId) -> Vec<StudyAlertEvent> {
        self.pending_alerts.remove(&study_id).unwrap_or_default()
    }

    /// Updates the read-only visible chart viewport context for one consumer and returns
    /// the consumer's studies that explicitly opted into chart-context recalculations.
    pub fn set_chart_context(
        &mut self,
        owner: ConsumerId,
        context: StudyChartContext,
    ) -> Vec<StudyInstanceId> {
        let previous = self.chart_contexts.insert(owner, context);
        if previous == Some(context) {
            return Vec::new();
        }
        self.studies
            .iter()
            .filter_map(|(study_id, node)| {
                (node.owner == Some(owner)
                    && node
                        .program
                        .is_some_and(|program| program.observes_chart_context))
                .then_some(*study_id)
            })
            .collect()
    }

    pub(crate) fn begin_turn(&self) {
        if let Ok(executor) = self.executor.get_or_init(execution::Executor::start) {
            executor.begin_turn();
        }
    }

    pub(crate) fn execution_isolated(&self) -> bool {
        self.executor.get().is_some_and(|executor| match executor {
            Ok(executor) => executor.is_busy(),
            Err(_) => true,
        })
    }

    fn ensure_execution_available(
        &self,
        study_id: StudyInstanceId,
    ) -> Result<(), StudyRuntimeError> {
        match self.executor.get() {
            Some(Err(detail)) => Err(StudyRuntimeError::ExecutionRejected {
                study_id,
                detail: detail.clone(),
            }),
            Some(Ok(executor)) if executor.is_busy() => Err(StudyRuntimeError::ExecutionRejected {
                study_id,
                detail: execution::BUSY_DETAIL.into(),
            }),
            _ => Ok(()),
        }
    }

    /// Returns the number of registered study instances.
    #[must_use]
    pub fn len(&self) -> usize {
        self.studies.len()
    }

    /// Returns whether no study instances are registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.studies.is_empty()
    }

    /// Returns one registered runtime-independent definition.
    #[must_use]
    pub fn definition(&self, study_id: StudyInstanceId) -> Option<&StudyDefinition> {
        self.studies.get(&study_id).map(|node| &node.definition)
    }

    /// Registers one study for an existing market consumer after validating all
    /// of its static dependencies.
    ///
    /// # Errors
    /// Returns an error for invalid identifiers, bounds, canonical market series, or references to
    /// outputs that are not already registered for the same consumer.
    pub(crate) fn register_native_for_consumer(
        &mut self,
        owner: ConsumerId,
        registration: NativeStudyRegistration,
    ) -> Result<StudyInstanceId, StudyRuntimeError> {
        self.register_node(
            Some(owner),
            registration.definition,
            registration.settings,
            Some(registration.program),
        )
    }

    /// Reinitializes one production study in place after a declared input or
    /// typed setting changes. The instance/output identities stay stable so
    /// downstream dependencies and presentation references do not need a second
    /// remapping layer.
    ///
    /// The output identifier sequence is the stable interface contract. Titles,
    /// plot kinds, pane placement, scales, settings, and market dependencies may
    /// change; removing/reordering an output requires a new study instance.
    pub(crate) fn reinitialize_native_for_consumer(
        &mut self,
        owner: ConsumerId,
        study_id: StudyInstanceId,
        registration: NativeStudyRegistration,
    ) -> Result<Vec<StudyInstanceId>, StudyRuntimeError> {
        let current = self
            .studies
            .get(&study_id)
            .ok_or(StudyRuntimeError::UnknownStudy(study_id))?;
        if current.owner != Some(owner) {
            return Err(StudyRuntimeError::UnknownStudy(study_id));
        }
        for dependency in &registration.definition.dependencies {
            if let StudyDependency::Output(output) = dependency
                && output.study_id >= study_id
            {
                return Err(StudyRuntimeError::DependencyOrderViolation {
                    study_id,
                    dependency: *output,
                });
            }
        }
        self.validate_definition(Some(owner), &registration.definition)?;
        validate_settings(&registration.definition.settings, &registration.settings)?;
        let same_output_interface = current.definition.outputs.len()
            == registration.definition.outputs.len()
            && current
                .definition
                .outputs
                .iter()
                .zip(&registration.definition.outputs)
                .all(|(left, right)| left.identifier == right.identifier);
        if !same_output_interface {
            return Err(StudyRuntimeError::OutputInterfaceChanged(study_id));
        }

        let affected = self.dependent_subtree(study_id)?;
        let PreparedStudyStates {
            states: mut fresh_states,
            total_bytes: replacement_state_bytes,
        } = self.fresh_states_for_reinitialization(
            study_id,
            &affected,
            registration.program,
            &registration.settings,
        )?;
        let (root_state, root_state_bytes, root_intrabar_state, root_intrabar_state_bytes) =
            fresh_states
                .remove(&study_id)
                .ok_or(StudyRuntimeError::UnknownStudy(study_id))?;
        self.studies.insert(
            study_id,
            StudyNode {
                owner: Some(owner),
                definition: registration.definition,
                settings: registration.settings,
                program: Some(registration.program),
                state: root_state,
                state_bytes: root_state_bytes,
                intrabar_state: root_intrabar_state,
                intrabar_state_bytes: root_intrabar_state_bytes,
            },
        );
        for (affected_id, (state, state_bytes, intrabar_state, intrabar_state_bytes)) in
            fresh_states
        {
            if let Some(node) = self.studies.get_mut(&affected_id) {
                node.state = state;
                node.state_bytes = state_bytes;
                node.intrabar_state = intrabar_state;
                node.intrabar_state_bytes = intrabar_state_bytes;
            }
        }
        self.state_bytes = replacement_state_bytes;
        self.rebuild_dependency_indexes();
        for affected_id in &affected {
            self.remove_outputs_for_study(*affected_id);
            self.remove_study_artifacts(*affected_id);
        }
        Ok(affected.into_iter().collect())
    }

    #[cfg(test)]
    fn register(
        &mut self,
        definition: StudyDefinition,
    ) -> Result<StudyInstanceId, StudyRuntimeError> {
        let settings = StudySettings::defaults(&definition.settings)?;
        self.register_node(None, definition, settings, None)
    }

    #[cfg(test)]
    fn register_for_consumer(
        &mut self,
        owner: ConsumerId,
        definition: StudyDefinition,
    ) -> Result<StudyInstanceId, StudyRuntimeError> {
        let settings = StudySettings::defaults(&definition.settings)?;
        self.register_node(Some(owner), definition, settings, None)
    }

    fn register_node(
        &mut self,
        owner: Option<ConsumerId>,
        definition: StudyDefinition,
        settings: StudySettings,
        program: Option<NativeStudyProgram>,
    ) -> Result<StudyInstanceId, StudyRuntimeError> {
        self.validate_definition(owner, &definition)?;
        validate_settings(&definition.settings, &settings)?;
        let state = Self::fresh_state(program, &settings)?;
        let state_bytes = Self::reported_state_bytes(state.as_ref())?;
        let intrabar_state = Self::fresh_intrabar_state(program, &settings)?;
        let intrabar_state_bytes = Self::reported_state_bytes(intrabar_state.as_ref())?;
        let combined_bytes = state_bytes
            .checked_add(intrabar_state_bytes)
            .ok_or(StudyRuntimeError::RangeOverflow)?;
        let next_state_bytes = self.validate_state_memory_bytes(combined_bytes, 0)?;
        if self.studies.len() == self.config.maximum_studies.get() {
            return Err(StudyRuntimeError::StudyLimitExceeded {
                maximum: self.config.maximum_studies.get(),
            });
        }

        let study_id = NonZeroU64::new(self.next_id)
            .map(StudyInstanceId)
            .ok_or(StudyRuntimeError::IdentifierExhausted)?;
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or(StudyRuntimeError::IdentifierExhausted)?;

        for dependency in &definition.dependencies {
            match dependency {
                StudyDependency::Market(input) => {
                    self.market_dependents
                        .entry(input.series.clone())
                        .or_default()
                        .insert(study_id);
                }
                StudyDependency::Output(output) => {
                    self.output_dependents
                        .entry(*output)
                        .or_default()
                        .insert(study_id);
                }
            }
        }
        self.studies.insert(
            study_id,
            StudyNode {
                owner,
                definition,
                settings,
                program,
                state,
                state_bytes,
                intrabar_state,
                intrabar_state_bytes,
            },
        );
        self.state_bytes = next_state_bytes;
        Ok(study_id)
    }

    /// Returns the market consumer that owns one production study instance.
    #[must_use]
    pub(crate) fn owner(&self, study_id: StudyInstanceId) -> Option<ConsumerId> {
        self.studies.get(&study_id).and_then(|node| node.owner)
    }

    #[cfg(test)]
    #[must_use]
    pub(crate) fn native_registration(
        &self,
        study_id: StudyInstanceId,
    ) -> Option<NativeStudyRegistration> {
        let node = self.studies.get(&study_id)?;
        Some(NativeStudyRegistration {
            definition: node.definition.clone(),
            settings: node.settings.clone(),
            program: node.program?,
        })
    }

    /// Returns production study identities owned by one chart consumer in
    /// dependency/registration order.
    #[must_use]
    pub(crate) fn owned_studies(&self, owner: ConsumerId) -> Vec<StudyInstanceId> {
        self.studies
            .iter()
            .filter_map(|(study_id, node)| (node.owner == Some(owner)).then_some(*study_id))
            .collect()
    }

    /// Removes every study owned by one market consumer.
    pub(crate) fn remove_consumer(&mut self, owner: ConsumerId) -> Vec<StudyInstanceId> {
        self.chart_contexts.remove(&owner);
        let removed = self
            .studies
            .iter()
            .filter_map(|(study_id, node)| (node.owner == Some(owner)).then_some(*study_id))
            .collect::<Vec<_>>();
        if removed.is_empty() {
            return removed;
        }
        for study_id in &removed {
            if let Some(node) = self.studies.remove(study_id) {
                self.state_bytes = self
                    .state_bytes
                    .saturating_sub(node.state_bytes.saturating_add(node.intrabar_state_bytes));
            }
            self.remove_outputs_for_study(*study_id);
            self.remove_study_artifacts(*study_id);
        }
        self.rebuild_dependency_indexes();
        removed
    }

    /// Removes one study and every downstream study that depends on one of its outputs.
    ///
    /// # Errors
    /// Returns [`StudyRuntimeError::UnknownStudy`] when `study_id` is not live.
    pub fn remove_subtree(
        &mut self,
        study_id: StudyInstanceId,
    ) -> Result<Vec<StudyInstanceId>, StudyRuntimeError> {
        if !self.studies.contains_key(&study_id) {
            return Err(StudyRuntimeError::UnknownStudy(study_id));
        }

        let mut removed = BTreeSet::from([study_id]);
        loop {
            let previous_len = removed.len();
            for (output, dependents) in &self.output_dependents {
                if removed.contains(&output.study_id) {
                    removed.extend(dependents.iter().copied());
                }
            }
            if removed.len() == previous_len {
                break;
            }
        }

        for id in &removed {
            if let Some(node) = self.studies.remove(id) {
                self.state_bytes = self
                    .state_bytes
                    .saturating_sub(node.state_bytes.saturating_add(node.intrabar_state_bytes));
            }
            self.remove_outputs_for_study(*id);
            self.remove_study_artifacts(*id);
        }
        self.rebuild_dependency_indexes();
        Ok(removed.into_iter().collect())
    }

    fn dependent_subtree(
        &self,
        study_id: StudyInstanceId,
    ) -> Result<BTreeSet<StudyInstanceId>, StudyRuntimeError> {
        if !self.studies.contains_key(&study_id) {
            return Err(StudyRuntimeError::UnknownStudy(study_id));
        }
        let mut affected = BTreeSet::from([study_id]);
        loop {
            let previous_len = affected.len();
            for (output, dependents) in &self.output_dependents {
                if affected.contains(&output.study_id) {
                    affected.extend(dependents.iter().copied());
                }
            }
            if affected.len() == previous_len {
                return Ok(affected);
            }
        }
    }

    pub(crate) fn checkpoint_subtree(
        &self,
        study_id: StudyInstanceId,
    ) -> Result<StudySubtreeCheckpoint, StudyRuntimeError> {
        let affected = self.dependent_subtree(study_id)?;
        let mut nodes = BTreeMap::new();
        for affected_id in &affected {
            let node = self
                .studies
                .get(affected_id)
                .ok_or(StudyRuntimeError::UnknownStudy(*affected_id))?;
            let cloned = catch_unwind(AssertUnwindSafe(|| node.clone())).map_err(|_| {
                StudyRuntimeError::ExecutionRejected {
                    study_id: *affected_id,
                    detail: "native study state clone panicked".to_string(),
                }
            })?;
            nodes.insert(*affected_id, cloned);
        }
        let outputs = self
            .outputs
            .iter()
            .filter(|(output, _)| affected.contains(&output.study_id))
            .map(|(output, series)| (*output, series.clone()))
            .collect();
        let style_overlays = self
            .style_overlays
            .iter()
            .filter(|(id, _)| affected.contains(id))
            .map(|(&id, overlay)| (id, overlay.clone()))
            .collect();
        let scenes = self
            .scenes
            .iter()
            .filter(|(id, _)| affected.contains(id))
            .map(|(&id, scene)| (id, scene.clone()))
            .collect();
        let alert_ledgers = self
            .alert_ledgers
            .iter()
            .filter(|(id, _)| affected.contains(id))
            .map(|(&id, ledger)| (id, ledger.clone()))
            .collect();
        Ok(StudySubtreeCheckpoint {
            nodes,
            outputs,
            style_overlays,
            scenes,
            alert_ledgers,
            output_points: self.output_points,
            state_bytes: self.state_bytes,
        })
    }

    pub(crate) fn restore_subtree_checkpoint(&mut self, checkpoint: StudySubtreeCheckpoint) {
        let affected = checkpoint.nodes.keys().copied().collect::<BTreeSet<_>>();
        for (study_id, node) in checkpoint.nodes {
            self.studies.insert(study_id, node);
        }
        self.outputs
            .retain(|output, _| !affected.contains(&output.study_id));
        self.outputs.extend(checkpoint.outputs);
        self.style_overlays.retain(|id, _| !affected.contains(id));
        self.style_overlays.extend(checkpoint.style_overlays);
        self.scenes.retain(|id, _| !affected.contains(id));
        self.scenes.extend(checkpoint.scenes);
        self.alert_ledgers.retain(|id, _| !affected.contains(id));
        self.alert_ledgers.extend(checkpoint.alert_ledgers);
        self.scene_deltas.retain(|id, _| !affected.contains(id));
        self.pending_alerts.retain(|id, _| !affected.contains(id));
        self.output_points = checkpoint.output_points;
        self.state_bytes = checkpoint.state_bytes;
        self.rebuild_dependency_indexes();
    }

    /// Returns the deduplicated upstream market demand required by all live studies.
    ///
    /// Multiple studies requesting the same canonical series share one entry whose stream set is
    /// the union of their requirements. `MarketEngine` remains responsible for validating provider
    /// capabilities and turning this demand into actual subscriptions.
    #[must_use]
    pub fn market_requirements(&self) -> BTreeMap<BarSeriesKey, StreamRequirements> {
        let mut requirements: BTreeMap<BarSeriesKey, StreamRequirements> = BTreeMap::new();
        for node in self.studies.values() {
            for dependency in &node.definition.dependencies {
                let StudyDependency::Market(input) = dependency else {
                    continue;
                };
                requirements
                    .entry(input.series.clone())
                    .and_modify(|streams| *streams = streams.union(input.streams))
                    .or_insert(input.streams);
            }
        }
        requirements
    }

    /// Returns the transitive market streams required by one study.
    ///
    /// Output dependencies inherit the requirements of their upstream study;
    /// this keeps a downstream scalar publication honest when its calculation
    /// is fed by a trade- or depth-backed study output.
    #[must_use]
    pub fn input_stream_requirements(
        &self,
        study_id: StudyInstanceId,
    ) -> Option<StreamRequirements> {
        fn collect(
            runtime: &StudyRuntime,
            study_id: StudyInstanceId,
            visiting: &mut BTreeSet<StudyInstanceId>,
        ) -> Option<StreamRequirements> {
            if !visiting.insert(study_id) {
                return None;
            }
            let node = runtime.studies.get(&study_id)?;
            let mut streams = StreamRequirements::NONE;
            for dependency in &node.definition.dependencies {
                match dependency {
                    StudyDependency::Market(input) => {
                        streams = streams.union(input.streams);
                    }
                    StudyDependency::Output(output) => {
                        streams = streams.union(collect(runtime, output.study_id, visiting)?);
                    }
                }
            }
            visiting.remove(&study_id);
            Some(streams)
        }

        collect(self, study_id, &mut BTreeSet::new())
    }

    /// Returns a zero-copy canonical market-series view for one study dependency.
    ///
    /// `Ok(None)` means the dependency is valid but canonical history has not
    /// arrived yet. The caller can therefore distinguish normal data loading from
    /// an invalid dependency index/type without inventing placeholder rows.
    ///
    /// # Errors
    /// Returns an error for an unknown study, an invalid dependency index, or a
    /// dependency that refers to another study output rather than market data.
    pub fn market_input(
        &self,
        engine: &MarketEngine,
        study_id: StudyInstanceId,
        dependency_index: usize,
    ) -> Result<Option<StudyMarketSeries>, StudyRuntimeError> {
        let node = self
            .studies
            .get(&study_id)
            .ok_or(StudyRuntimeError::UnknownStudy(study_id))?;
        let dependency = node.definition.dependencies.get(dependency_index).ok_or(
            StudyRuntimeError::DependencyIndexOutOfBounds {
                study_id,
                dependency_index,
            },
        )?;
        let StudyDependency::Market(input) = dependency else {
            return Err(StudyRuntimeError::DependencyIsNotMarket {
                study_id,
                dependency_index,
            });
        };
        Ok(engine
            .series_view(&input.series)
            .map(StudyMarketSeries::new))
    }

    /// Returns one committed native numeric output series.
    #[must_use]
    pub fn output_series(&self, output: StudyOutputId) -> Option<&StudyOutputSeries> {
        self.outputs.get(&output)
    }

    /// Executes one native study when every declared dependency is currently
    /// available. The first dependency owns the output timeline; secondary
    /// inputs retain their own timestamps and must be aligned explicitly by the
    /// study implementation.
    ///
    /// `Ok(false)` means at least one input is still loading. No placeholder
    /// output is committed in that case.
    ///
    /// # Errors
    /// Returns an error for unknown/non-executable studies, output-memory bounds,
    /// output-generation exhaustion, or a calculation rejection.
    #[cfg(test)]
    pub(crate) fn execute_ready(
        &mut self,
        engine: &MarketEngine,
        study_id: StudyInstanceId,
    ) -> Result<bool, StudyRuntimeError> {
        let mut no_live_market = |_input: &StudyMarketInput| None;
        self.execute_ready_with_live(engine, study_id, &mut no_live_market)
    }

    pub(crate) fn execute_ready_with_live<'a, F>(
        &mut self,
        engine: &MarketEngine,
        study_id: StudyInstanceId,
        live_market: &mut F,
    ) -> Result<bool, StudyRuntimeError>
    where
        F: FnMut(&StudyMarketInput) -> Option<StudyLiveMarketData<'a>>,
    {
        self.execute_ready_range_with_live(engine, study_id, None, live_market)
    }

    /// Re-executes one study and every downstream dependent whose inputs become
    /// ready, preserving dependency/registration order. This is the covering
    /// path used immediately after in-place study reinitialization.
    #[cfg(test)]
    pub(crate) fn execute_ready_subtree(
        &mut self,
        engine: &MarketEngine,
        study_id: StudyInstanceId,
    ) -> Result<Vec<StudyInstanceId>, StudyRuntimeError> {
        let mut no_live_market = |_input: &StudyMarketInput| None;
        self.execute_ready_subtree_with_live(engine, study_id, &mut no_live_market)
    }

    pub(crate) fn execute_ready_subtree_with_live<'a, F>(
        &mut self,
        engine: &MarketEngine,
        study_id: StudyInstanceId,
        live_market: &mut F,
    ) -> Result<Vec<StudyInstanceId>, StudyRuntimeError>
    where
        F: FnMut(&StudyMarketInput) -> Option<StudyLiveMarketData<'a>>,
    {
        let affected = self.dependent_subtree(study_id)?;
        let mut executed = Vec::new();
        for candidate in affected {
            match self.execute_ready_with_live(engine, candidate, live_market) {
                Ok(true) => executed.push(candidate),
                Ok(false) | Err(StudyRuntimeError::StudyNotExecutable(_)) => {}
                Err(error) => return Err(error),
            }
        }
        Ok(executed)
    }

    #[cfg(test)]
    fn execute_ready_range(
        &mut self,
        engine: &MarketEngine,
        study_id: StudyInstanceId,
        requested_dirty: Option<StudyDirtyRange>,
    ) -> Result<bool, StudyRuntimeError> {
        let mut no_live_market = |_input: &StudyMarketInput| None;
        self.execute_ready_range_with_live(engine, study_id, requested_dirty, &mut no_live_market)
    }

    fn execute_ready_range_with_live<'a, F>(
        &mut self,
        engine: &MarketEngine,
        study_id: StudyInstanceId,
        requested_dirty: Option<StudyDirtyRange>,
        live_market: &mut F,
    ) -> Result<bool, StudyRuntimeError>
    where
        F: FnMut(&StudyMarketInput) -> Option<StudyLiveMarketData<'a>>,
    {
        self.ensure_execution_available(study_id)?;
        let (owner, definition, settings, program, previous_state_bytes) = {
            let node = self
                .studies
                .get(&study_id)
                .ok_or(StudyRuntimeError::UnknownStudy(study_id))?;
            (
                node.owner,
                node.definition.clone(),
                node.settings.clone(),
                node.program
                    .ok_or(StudyRuntimeError::StudyNotExecutable(study_id))?,
                node.state_bytes.saturating_add(node.intrabar_state_bytes),
            )
        };
        let Some(inputs) = self.resolve_inputs(engine, &definition) else {
            return Ok(false);
        };
        let Some(live_inputs) = Self::resolve_live_inputs(&definition, live_market) else {
            return Ok(false);
        };
        let timeline =
            primary_timeline(inputs.first().ok_or(StudyRuntimeError::MissingDependency)?);
        if timeline.len() == 0 {
            return Ok(false);
        }
        if timeline.len() > self.config.maximum_points_per_output.get() {
            return Err(StudyRuntimeError::OutputPointLimitExceeded {
                maximum: self.config.maximum_points_per_output.get(),
                requested: timeline.len(),
            });
        }
        let output_count = definition.outputs.len();
        let total_points =
            self.output_points_after_replacement(study_id, timeline.len(), output_count)?;
        let next_generation = self
            .next_output_generation
            .checked_add(1)
            .ok_or(StudyRuntimeError::OutputGenerationExhausted)?;
        let generation = self.next_output_generation;
        let dirty = if let Some(requested) = requested_dirty {
            let Some(clamped) = clamp_dirty_range(requested, timeline.len()) else {
                return Ok(false);
            };
            clamped
        } else {
            StudyDirtyRange {
                start: 0,
                end_exclusive: Some(timeline.len()),
            }
        };
        let covering_execution = dirty.start == 0 && dirty.end_exclusive == Some(timeline.len());
        let meta = self.build_execution_meta(study_id, &inputs, &timeline, covering_execution);
        let chart_context = owner
            .and_then(|id| self.chart_contexts.get(&id).copied())
            .unwrap_or_default();
        let (mut candidate_state, mut candidate_intrabar_state) =
            self.prepare_candidate_states(study_id, program, &settings, covering_execution)?;
        let calculated = self.calculate_outputs(
            &NativeStudyCalculation {
                study_id,
                settings: &settings,
                program,
                output_count,
                inputs: &inputs,
                live_inputs: &live_inputs,
                timeline: &timeline,
                dirty,
                meta,
                chart_context,
            },
            &mut candidate_state,
            &mut candidate_intrabar_state,
        )?;
        let candidate_state_bytes = Self::reported_state_bytes(candidate_state.as_ref())?;
        let candidate_intrabar_bytes =
            Self::reported_state_bytes(candidate_intrabar_state.as_ref())?;
        let combined_candidate_bytes = candidate_state_bytes
            .checked_add(candidate_intrabar_bytes)
            .ok_or(StudyRuntimeError::RangeOverflow)?;
        let next_state_bytes =
            self.validate_state_memory_bytes(combined_candidate_bytes, previous_state_bytes)?;
        if let Some(live) = self.studies.get_mut(&study_id) {
            live.state = candidate_state;
            live.state_bytes = candidate_state_bytes;
            live.intrabar_state = candidate_intrabar_state;
            live.intrabar_state_bytes = candidate_intrabar_bytes;
        }
        self.state_bytes = next_state_bytes;
        self.commit_wave_artifacts(
            study_id,
            &timeline,
            calculated,
            generation,
            total_points,
            meta.is_confirmed_bar,
        );
        self.next_output_generation = next_generation;
        Ok(true)
    }

    fn build_execution_meta(
        &self,
        study_id: StudyInstanceId,
        inputs: &[ResolvedStudyInput],
        timeline: &StudyOutputTimeline,
        covering_execution: bool,
    ) -> StudyExecutionMeta {
        let previous_len = self
            .outputs
            .get(&StudyOutputId {
                study_id,
                output_index: 0,
            })
            .map_or(0, StudyOutputSeries::len);
        let (is_confirmed_bar, bar_period) = match inputs.first() {
            Some(ResolvedStudyInput::Market(market)) => {
                (!market.forming(), Some(market.series().period))
            }
            _ => (true, None),
        };
        let evaluation_time_unix_nanos = timeline
            .timestamp(timeline.len().saturating_sub(1))
            .unwrap_or(0);
        StudyExecutionMeta {
            is_historical: covering_execution,
            is_new_bar: timeline.len() > previous_len,
            is_confirmed_bar,
            evaluation_time_unix_nanos,
            bar_count: timeline.len(),
            bar_period,
        }
    }

    fn prepare_candidate_states(
        &self,
        study_id: StudyInstanceId,
        program: NativeStudyProgram,
        settings: &StudySettings,
        covering_execution: bool,
    ) -> Result<(Option<NativeStudyState>, Option<NativeStudyState>), StudyRuntimeError> {
        let candidate_state = if covering_execution {
            Self::fresh_state(Some(program), settings)?
        } else {
            catch_unwind(AssertUnwindSafe(|| {
                self.studies
                    .get(&study_id)
                    .and_then(|node| node.state.clone())
            }))
            .map_err(|_| StudyRuntimeError::ExecutionRejected {
                study_id,
                detail: "native study state clone panicked".to_string(),
            })?
        };
        let candidate_intrabar_state = if covering_execution {
            Self::fresh_intrabar_state(Some(program), settings)?
        } else {
            catch_unwind(AssertUnwindSafe(|| {
                self.studies
                    .get(&study_id)
                    .and_then(|node| node.intrabar_state.clone())
            }))
            .map_err(|_| StudyRuntimeError::ExecutionRejected {
                study_id,
                detail: "native study intrabar state clone panicked".to_string(),
            })?
        };
        Ok((candidate_state, candidate_intrabar_state))
    }

    fn commit_wave_artifacts(
        &mut self,
        study_id: StudyInstanceId,
        timeline: &StudyOutputTimeline,
        calculated: CalculatedStudyOutputs,
        generation: u64,
        total_points: usize,
        is_confirmed_bar: bool,
    ) {
        self.commit_outputs(
            study_id,
            timeline,
            calculated.outputs,
            generation,
            total_points,
        );
        self.style_overlays
            .insert(study_id, calculated.style_overlay);
        let deltas = self.scenes.get(&study_id).map_or_else(
            || StudySceneState::default().diff(&calculated.scene),
            |previous| previous.diff(&calculated.scene),
        );
        if !deltas.is_empty() {
            self.scene_deltas
                .entry(study_id)
                .or_default()
                .extend(deltas);
        }
        self.scenes.insert(study_id, calculated.scene);
        if !calculated.alerts.is_empty() {
            let committed_alerts = self
                .alert_ledgers
                .entry(study_id)
                .or_default()
                .commit_wave(calculated.alerts, is_confirmed_bar);
            if !committed_alerts.is_empty() {
                self.pending_alerts
                    .entry(study_id)
                    .or_default()
                    .extend(committed_alerts);
            }
        }
        #[cfg(test)]
        {
            self.last_output_preparation_work_rows = calculated.preparation_work_rows;
        }
    }

    fn output_points_after_replacement(
        &self,
        study_id: StudyInstanceId,
        rows: usize,
        output_count: usize,
    ) -> Result<usize, StudyRuntimeError> {
        let requested_points = rows
            .checked_mul(output_count)
            .ok_or(StudyRuntimeError::RangeOverflow)?;
        let current_points = self
            .outputs
            .iter()
            .filter(|(output, _)| output.study_id == study_id)
            .map(|(_, series)| series.len())
            .try_fold(0usize, usize::checked_add)
            .ok_or(StudyRuntimeError::RangeOverflow)?;
        let total_points = self
            .output_points
            .checked_sub(current_points)
            .and_then(|points| points.checked_add(requested_points))
            .ok_or(StudyRuntimeError::RangeOverflow)?;
        if total_points > self.config.maximum_total_output_points.get() {
            return Err(StudyRuntimeError::TotalOutputPointLimitExceeded {
                maximum: self.config.maximum_total_output_points.get(),
                requested: total_points,
            });
        }
        Ok(total_points)
    }

    fn validate_state_memory_bytes(
        &self,
        requested: usize,
        replaced_bytes: usize,
    ) -> Result<usize, StudyRuntimeError> {
        if requested > self.config.maximum_state_bytes_per_study.get() {
            return Err(StudyRuntimeError::StateMemoryLimitExceeded {
                maximum: self.config.maximum_state_bytes_per_study.get(),
                requested,
            });
        }
        let total = self
            .state_bytes
            .checked_sub(replaced_bytes)
            .and_then(|bytes| bytes.checked_add(requested))
            .ok_or(StudyRuntimeError::RangeOverflow)?;
        if total > self.config.maximum_total_state_bytes.get() {
            return Err(StudyRuntimeError::TotalStateMemoryLimitExceeded {
                maximum: self.config.maximum_total_state_bytes.get(),
                requested: total,
            });
        }
        Ok(total)
    }

    fn reported_state_bytes(state: Option<&NativeStudyState>) -> Result<usize, StudyRuntimeError> {
        let Some(state) = state else {
            return Ok(0);
        };
        catch_unwind(AssertUnwindSafe(|| state.runtime_bytes())).map_err(|_| {
            StudyRuntimeError::StateInitializationRejected {
                detail: "native study state accounting panicked".to_string(),
            }
        })
    }

    fn fresh_state(
        program: Option<NativeStudyProgram>,
        settings: &StudySettings,
    ) -> Result<Option<NativeStudyState>, StudyRuntimeError> {
        let Some(factory) = program.and_then(|program| program.state_factory) else {
            return Ok(None);
        };
        match catch_unwind(AssertUnwindSafe(|| factory(settings))) {
            Ok(Ok(state)) => Ok(Some(state)),
            Ok(Err(detail)) => Err(StudyRuntimeError::StateInitializationRejected {
                detail: bounded_execution_detail(detail),
            }),
            Err(_) => Err(StudyRuntimeError::StateInitializationRejected {
                detail: "native study state factory panicked".to_string(),
            }),
        }
    }

    fn fresh_intrabar_state(
        program: Option<NativeStudyProgram>,
        settings: &StudySettings,
    ) -> Result<Option<NativeStudyState>, StudyRuntimeError> {
        let Some(factory) = program.and_then(|program| program.intrabar_state_factory) else {
            return Ok(None);
        };
        match catch_unwind(AssertUnwindSafe(|| factory(settings))) {
            Ok(Ok(state)) => Ok(Some(state)),
            Ok(Err(detail)) => Err(StudyRuntimeError::StateInitializationRejected {
                detail: bounded_execution_detail(detail),
            }),
            Err(_) => Err(StudyRuntimeError::StateInitializationRejected {
                detail: "native study intrabar state factory panicked".to_string(),
            }),
        }
    }

    fn fresh_states_for_reinitialization(
        &self,
        study_id: StudyInstanceId,
        affected: &BTreeSet<StudyInstanceId>,
        root_program: NativeStudyProgram,
        root_settings: &StudySettings,
    ) -> Result<PreparedStudyStates, StudyRuntimeError> {
        let replaced_bytes = affected
            .iter()
            .filter_map(|affected_id| self.studies.get(affected_id))
            .map(|node| node.state_bytes.saturating_add(node.intrabar_state_bytes))
            .try_fold(0usize, usize::checked_add)
            .ok_or(StudyRuntimeError::RangeOverflow)?;
        let mut total = self
            .state_bytes
            .checked_sub(replaced_bytes)
            .ok_or(StudyRuntimeError::RangeOverflow)?;
        let mut states = BTreeMap::new();
        for affected_id in affected {
            let (state, intrabar_state) = if *affected_id == study_id {
                (
                    Self::fresh_state(Some(root_program), root_settings)?,
                    Self::fresh_intrabar_state(Some(root_program), root_settings)?,
                )
            } else {
                let node = self
                    .studies
                    .get(affected_id)
                    .ok_or(StudyRuntimeError::UnknownStudy(*affected_id))?;
                (
                    Self::fresh_state(node.program, &node.settings)?,
                    Self::fresh_intrabar_state(node.program, &node.settings)?,
                )
            };
            let state_bytes = Self::reported_state_bytes(state.as_ref())?;
            let intrabar_bytes = Self::reported_state_bytes(intrabar_state.as_ref())?;
            let requested = state_bytes
                .checked_add(intrabar_bytes)
                .ok_or(StudyRuntimeError::RangeOverflow)?;
            if requested > self.config.maximum_state_bytes_per_study.get() {
                return Err(StudyRuntimeError::StateMemoryLimitExceeded {
                    maximum: self.config.maximum_state_bytes_per_study.get(),
                    requested,
                });
            }
            total = total
                .checked_add(requested)
                .ok_or(StudyRuntimeError::RangeOverflow)?;
            states.insert(
                *affected_id,
                (state, state_bytes, intrabar_state, intrabar_bytes),
            );
        }
        if total > self.config.maximum_total_state_bytes.get() {
            return Err(StudyRuntimeError::TotalStateMemoryLimitExceeded {
                maximum: self.config.maximum_total_state_bytes.get(),
                requested: total,
            });
        }
        Ok(PreparedStudyStates {
            states,
            total_bytes: total,
        })
    }

    fn calculate_outputs(
        &self,
        calculation: &NativeStudyCalculation<'_>,
        state: &mut Option<NativeStudyState>,
        intrabar_state: &mut Option<NativeStudyState>,
    ) -> Result<CalculatedStudyOutputs, StudyRuntimeError> {
        let prepared = (0..calculation.output_count)
            .map(|output_index| {
                let output = StudyOutputId {
                    study_id: calculation.study_id,
                    output_index,
                };
                StudyOutputBuffer::from_previous(
                    calculation.timeline,
                    self.outputs.get(&output),
                    calculation.dirty,
                )
            })
            .collect::<Vec<_>>();
        #[cfg(test)]
        let preparation_work_rows = prepared
            .iter()
            .map(|(_, work_rows)| *work_rows)
            .sum::<usize>();
        let outputs = prepared
            .into_iter()
            .map(|(output, _)| output)
            .collect::<Vec<_>>();
        let mut style_overlay = self
            .style_overlays
            .get(&calculation.study_id)
            .cloned()
            .unwrap_or_default();
        style_overlay.prepare_for_dirty(calculation.timeline.len(), calculation.dirty);
        let covering = calculation.dirty.start == 0
            && calculation.dirty.end_exclusive == Some(calculation.timeline.len());
        let scene = if covering {
            StudySceneState::default()
        } else {
            self.scenes
                .get(&calculation.study_id)
                .cloned()
                .unwrap_or_default()
        };
        let executor = self
            .executor
            .get_or_init(execution::Executor::start)
            .as_ref()
            .map_err(|detail| StudyRuntimeError::ExecutionRejected {
                study_id: calculation.study_id,
                detail: detail.clone(),
            })?;
        let result = executor
            .calculate(
                calculation,
                outputs,
                style_overlay,
                scene,
                state.take(),
                intrabar_state.take(),
            )
            .map_err(|detail| StudyRuntimeError::ExecutionRejected {
                study_id: calculation.study_id,
                detail,
            })?;
        *state = result.state;
        *intrabar_state = result.intrabar_state;
        Ok(CalculatedStudyOutputs {
            outputs: result.outputs,
            style_overlay: result.style_overlay,
            scene: result.scene,
            alerts: result.alerts,
            #[cfg(test)]
            preparation_work_rows,
        })
    }

    fn commit_outputs(
        &mut self,
        study_id: StudyInstanceId,
        timeline: &StudyOutputTimeline,
        outputs: Vec<StudyOutputBuffer>,
        generation: u64,
        total_points: usize,
    ) {
        self.remove_outputs_for_study(study_id);
        for (output_index, output) in outputs.into_iter().enumerate() {
            let (values, point_colors) = output.into_parts();
            self.outputs.insert(
                StudyOutputId {
                    study_id,
                    output_index,
                },
                StudyOutputSeries {
                    timeline: timeline.clone(),
                    values,
                    point_colors: Arc::new(point_colors),
                    materialized_values: Arc::new(OnceLock::new()),
                    generation,
                },
            );
        }
        self.output_points = total_points;
    }

    /// Incrementally executes every ready study affected by one accepted
    /// canonical live-bar mutation. The changed event is mapped to each study's
    /// own primary timeline by exact exchange timestamp before invalidation is
    /// applied, so secondary MTF inputs never reuse another series' row index.
    #[cfg(test)]
    pub(crate) fn execute_live_market_change(
        &mut self,
        engine: &MarketEngine,
        series: &BarSeriesKey,
        exchange_timestamp_unix_nanos: i64,
    ) -> Result<Vec<StudyInstanceId>, StudyRuntimeError> {
        let mut no_live_market = |_input: &StudyMarketInput| None;
        let batch = self.execute_live_market_change_with_live(
            engine,
            series,
            exchange_timestamp_unix_nanos,
            &mut no_live_market,
        )?;
        if let Some(error) = batch.errors.into_iter().next() {
            Err(error)
        } else {
            Ok(batch.executed)
        }
    }

    pub(crate) fn execute_live_market_change_with_live<'a, F>(
        &mut self,
        engine: &MarketEngine,
        series: &BarSeriesKey,
        exchange_timestamp_unix_nanos: i64,
        live_market: &mut F,
    ) -> Result<StudyExecutionBatch, StudyRuntimeError>
    where
        F: FnMut(&StudyMarketInput) -> Option<StudyLiveMarketData<'a>>,
    {
        let direct = self
            .market_dependents
            .get(series)
            .cloned()
            .unwrap_or_default();
        let mut pending = BTreeMap::<StudyInstanceId, StudyDirtyRange>::new();
        for study_id in direct {
            let Some(range) = self.market_change_range(
                engine,
                study_id,
                series,
                MarketStream::Bars,
                exchange_timestamp_unix_nanos,
            )?
            else {
                continue;
            };
            pending
                .entry(study_id)
                .and_modify(|current| *current = current.merge(range))
                .or_insert(range);
        }

        self.execute_pending_ranges_with_live(engine, pending, live_market, None)
    }

    pub(crate) fn execute_live_non_bar_change_with_live<'a, F, R>(
        &mut self,
        engine: &MarketEngine,
        change: StudyNonBarChange<'_>,
        live_market: &mut F,
        market_ready: &mut R,
    ) -> Result<StudyExecutionBatch, StudyRuntimeError>
    where
        F: FnMut(&StudyMarketInput) -> Option<StudyLiveMarketData<'a>>,
        R: FnMut(&StudyMarketInput) -> bool,
    {
        debug_assert!(!matches!(change.stream, MarketStream::Bars));
        let matching_series = self
            .market_dependents
            .keys()
            .filter(|series| {
                series.provider_id == change.provider_id
                    && series.instrument_id == change.instrument_id
                    && series.entitlement_id == change.entitlement_id
            })
            .cloned()
            .collect::<Vec<_>>();
        let mut pending = BTreeMap::<StudyInstanceId, StudyDirtyRange>::new();
        for series in matching_series {
            let direct = self
                .market_dependents
                .get(&series)
                .cloned()
                .unwrap_or_default();
            for study_id in direct {
                let Some(range) = self.market_change_range(
                    engine,
                    study_id,
                    &series,
                    change.stream,
                    change.observed_unix_nanos,
                )?
                else {
                    continue;
                };
                pending
                    .entry(study_id)
                    .and_modify(|current| *current = current.merge(range))
                    .or_insert(range);
            }
        }
        self.execute_pending_ranges_with_live(engine, pending, live_market, Some(market_ready))
    }

    /// Incrementally executes studies affected by one successful ranged
    /// historical repair. The supplied bounds are the actual provider-returned
    /// bar timestamps, not merely the requested viewport, so unchanged retained
    /// history stays outside the dirty range.
    pub(crate) fn execute_history_range_change_with_live<'a, F>(
        &mut self,
        engine: &MarketEngine,
        series: &BarSeriesKey,
        first_changed_unix_nanos: i64,
        last_changed_unix_nanos: i64,
        live_market: &mut F,
    ) -> Result<StudyExecutionBatch, StudyRuntimeError>
    where
        F: FnMut(&StudyMarketInput) -> Option<StudyLiveMarketData<'a>>,
    {
        if first_changed_unix_nanos > last_changed_unix_nanos {
            return Ok(StudyExecutionBatch::default());
        }
        let direct = self
            .market_dependents
            .get(series)
            .cloned()
            .unwrap_or_default();
        let mut pending = BTreeMap::<StudyInstanceId, StudyDirtyRange>::new();
        for study_id in direct {
            let Some(range) = self.market_history_change_range(
                engine,
                study_id,
                series,
                first_changed_unix_nanos,
                last_changed_unix_nanos,
            )?
            else {
                continue;
            };
            pending
                .entry(study_id)
                .and_modify(|current| *current = current.merge(range))
                .or_insert(range);
        }
        self.execute_pending_ranges_with_live(engine, pending, live_market, None)
    }

    fn execute_pending_ranges_with_live<'a, F>(
        &mut self,
        engine: &MarketEngine,
        mut pending: BTreeMap<StudyInstanceId, StudyDirtyRange>,
        live_market: &mut F,
        mut market_ready: Option<&mut dyn FnMut(&StudyMarketInput) -> bool>,
    ) -> Result<StudyExecutionBatch, StudyRuntimeError>
    where
        F: FnMut(&StudyMarketInput) -> Option<StudyLiveMarketData<'a>>,
    {
        let ordered = self.studies.keys().copied().collect::<Vec<_>>();
        let mut batch = StudyExecutionBatch::default();
        let mut blocked = BTreeSet::new();
        for study_id in ordered {
            let Some(range) = pending.remove(&study_id) else {
                continue;
            };
            if blocked.contains(&study_id) {
                continue;
            }
            if let Some(readiness) = market_ready.as_deref_mut() {
                let ready = self.study_market_ancestry_ready(study_id, readiness)?;
                if !ready {
                    blocked.extend(self.dependent_subtree(study_id)?);
                    continue;
                }
            }
            match self.execute_ready_range_with_live(engine, study_id, Some(range), live_market) {
                Ok(true) => {
                    batch.executed.push(study_id);
                    if let Err(error) =
                        self.propagate_output_change(engine, study_id, range, &mut pending)
                    {
                        batch.errors.push(error);
                        blocked.extend(self.dependent_subtree(study_id)?);
                    }
                }
                Ok(false) | Err(StudyRuntimeError::StudyNotExecutable(_)) => {
                    blocked.extend(self.dependent_subtree(study_id)?);
                }
                Err(error) => {
                    batch.errors.push(error);
                    blocked.extend(self.dependent_subtree(study_id)?);
                }
            }
        }
        Ok(batch)
    }

    fn study_market_ancestry_ready(
        &self,
        study_id: StudyInstanceId,
        readiness: &mut dyn FnMut(&StudyMarketInput) -> bool,
    ) -> Result<bool, StudyRuntimeError> {
        let mut pending = vec![study_id];
        let mut visited = BTreeSet::new();
        while let Some(candidate) = pending.pop() {
            if !visited.insert(candidate) {
                continue;
            }
            let node = self
                .studies
                .get(&candidate)
                .ok_or(StudyRuntimeError::UnknownStudy(candidate))?;
            for dependency in &node.definition.dependencies {
                match dependency {
                    StudyDependency::Market(input) => {
                        if !readiness(input) {
                            return Ok(false);
                        }
                    }
                    StudyDependency::Output(output) => pending.push(output.study_id),
                }
            }
        }
        Ok(true)
    }

    fn market_change_range(
        &self,
        engine: &MarketEngine,
        study_id: StudyInstanceId,
        changed_series: &BarSeriesKey,
        changed_stream: MarketStream,
        observed_unix_nanos: i64,
    ) -> Result<Option<StudyDirtyRange>, StudyRuntimeError> {
        let node = self
            .studies
            .get(&study_id)
            .ok_or(StudyRuntimeError::UnknownStudy(study_id))?;
        let mut dirty = None;
        for (dependency_index, dependency) in node.definition.dependencies.iter().enumerate() {
            let StudyDependency::Market(input) = dependency else {
                continue;
            };
            if input.series != *changed_series || !input.streams.contains(changed_stream) {
                continue;
            }
            let Some(length) = self.primary_timeline_len(engine, &node.definition) else {
                continue;
            };
            let start = if matches!(changed_stream, MarketStream::Bars) {
                let Some(start) =
                    self.primary_lower_bound(engine, &node.definition, observed_unix_nanos)
                else {
                    continue;
                };
                start
            } else {
                let Some(start) =
                    self.primary_row_containing(engine, &node.definition, observed_unix_nanos)
                else {
                    continue;
                };
                start
            };
            if start >= length {
                continue;
            }
            let candidate =
                if !matches!(changed_stream, MarketStream::Bars) || dependency_index == 0 {
                    StudyDirtyRange::bounded(start, start + 1)?
                        .invalidated_by(node.definition.invalidation)?
                } else {
                    // A secondary input may affect every later primary row through
                    // as-of/event-time alignment. Without assuming two series share
                    // row indexes, tail invalidation is the smallest safe default.
                    StudyDirtyRange::to_tail(start)
                };
            dirty = Some(dirty.map_or(candidate, |current: StudyDirtyRange| {
                current.merge(candidate)
            }));
        }
        Ok(dirty)
    }

    fn market_history_change_range(
        &self,
        engine: &MarketEngine,
        study_id: StudyInstanceId,
        changed_series: &BarSeriesKey,
        first_changed_unix_nanos: i64,
        last_changed_unix_nanos: i64,
    ) -> Result<Option<StudyDirtyRange>, StudyRuntimeError> {
        let node = self
            .studies
            .get(&study_id)
            .ok_or(StudyRuntimeError::UnknownStudy(study_id))?;
        let mut dirty = None;
        for (dependency_index, dependency) in node.definition.dependencies.iter().enumerate() {
            let StudyDependency::Market(input) = dependency else {
                continue;
            };
            if input.series != *changed_series || !input.streams.contains(MarketStream::Bars) {
                continue;
            }
            let Some(length) = self.primary_timeline_len(engine, &node.definition) else {
                continue;
            };
            let Some(start) =
                self.primary_lower_bound(engine, &node.definition, first_changed_unix_nanos)
            else {
                continue;
            };
            if start >= length {
                continue;
            }
            let candidate = if dependency_index == 0 {
                let Some(end_exclusive) =
                    self.primary_upper_bound(engine, &node.definition, last_changed_unix_nanos)
                else {
                    continue;
                };
                let end_exclusive = end_exclusive.min(length);
                if start >= end_exclusive {
                    continue;
                }
                StudyDirtyRange::bounded(start, end_exclusive)?
                    .invalidated_by(node.definition.invalidation)?
            } else {
                // Historical changes in a secondary time-aligned input can alter
                // every later primary row that resolves that input as-of time.
                StudyDirtyRange::to_tail(start)
            };
            dirty = Some(dirty.map_or(candidate, |current: StudyDirtyRange| {
                current.merge(candidate)
            }));
        }
        Ok(dirty)
    }

    fn primary_row_containing(
        &self,
        engine: &MarketEngine,
        definition: &StudyDefinition,
        observed_unix_nanos: i64,
    ) -> Option<usize> {
        let dependency = definition.dependencies.first()?;
        let index = match dependency {
            StudyDependency::Market(input) => engine
                .series_segments(&input.series)?
                .partition_point(|bar| bar.exchange_timestamp_unix_nanos <= observed_unix_nanos)
                .checked_sub(1)?,
            StudyDependency::Output(output) => self
                .outputs
                .get(output)?
                .timeline
                .upper_bound(observed_unix_nanos)
                .checked_sub(1)?,
        };
        let (bar, next, forming, period) = match dependency {
            StudyDependency::Market(input) => {
                let segments = engine.series_segments(&input.series)?;
                (
                    segments.get(index)?,
                    segments.get(index + 1),
                    segments.forming,
                    input.series.period,
                )
            }
            StudyDependency::Output(output) => {
                let view = &self.outputs.get(output)?.timeline.0.view;
                (
                    *view.get(index)?,
                    view.get(index + 1).copied(),
                    view.forming,
                    view.series().period,
                )
            }
        };
        if let Some(duration) = period.duration_nanos() {
            return bar
                .exchange_timestamp_unix_nanos
                .checked_add(duration)
                .is_some_and(|end_exclusive| observed_unix_nanos < end_exclusive)
                .then_some(index);
        }
        if let Some(next) = next {
            return (observed_unix_nanos < next.exchange_timestamp_unix_nanos).then_some(index);
        }
        // Tick/calendar periods do not expose a fixed nanosecond duration. The
        // newest row can own later live non-bar events only while it is explicitly
        // the forming canonical tail; a completed last row must not absorb an
        // arbitrary future event.
        (forming || observed_unix_nanos == bar.exchange_timestamp_unix_nanos).then_some(index)
    }

    fn propagate_output_change(
        &self,
        engine: &MarketEngine,
        source_study: StudyInstanceId,
        source_range: StudyDirtyRange,
        pending: &mut BTreeMap<StudyInstanceId, StudyDirtyRange>,
    ) -> Result<(), StudyRuntimeError> {
        let source_node = self
            .studies
            .get(&source_study)
            .ok_or(StudyRuntimeError::UnknownStudy(source_study))?;
        for output_index in 0..source_node.definition.outputs.len() {
            let output_id = StudyOutputId {
                study_id: source_study,
                output_index,
            };
            let Some(source_output) = self.outputs.get(&output_id) else {
                continue;
            };
            let Some(source_range) = clamp_dirty_range(source_range, source_output.len()) else {
                continue;
            };
            let Some(change_timestamp) = source_output.timestamp_unix_nanos(source_range.start)
            else {
                continue;
            };
            let dependents = self
                .output_dependents
                .get(&output_id)
                .cloned()
                .unwrap_or_default();
            for dependent in dependents {
                let node = self
                    .studies
                    .get(&dependent)
                    .ok_or(StudyRuntimeError::UnknownStudy(dependent))?;
                let mut dependent_dirty = None;
                for (dependency_index, dependency) in
                    node.definition.dependencies.iter().enumerate()
                {
                    if *dependency != StudyDependency::Output(output_id) {
                        continue;
                    }
                    let candidate = if dependency_index == 0 {
                        source_range.invalidated_by(node.definition.invalidation)?
                    } else {
                        let Some(length) = self.primary_timeline_len(engine, &node.definition)
                        else {
                            continue;
                        };
                        let Some(start) =
                            self.primary_lower_bound(engine, &node.definition, change_timestamp)
                        else {
                            continue;
                        };
                        if start >= length {
                            continue;
                        }
                        StudyDirtyRange::to_tail(start)
                    };
                    dependent_dirty = Some(
                        dependent_dirty.map_or(candidate, |current: StudyDirtyRange| {
                            current.merge(candidate)
                        }),
                    );
                }
                if let Some(range) = dependent_dirty {
                    pending
                        .entry(dependent)
                        .and_modify(|current| *current = current.merge(range))
                        .or_insert(range);
                }
            }
        }
        Ok(())
    }

    fn primary_timeline_len(
        &self,
        engine: &MarketEngine,
        definition: &StudyDefinition,
    ) -> Option<usize> {
        match definition.dependencies.first()? {
            StudyDependency::Market(input) => engine.series_bar_count(&input.series),
            StudyDependency::Output(output) => Some(self.outputs.get(output)?.len()),
        }
    }

    fn primary_lower_bound(
        &self,
        engine: &MarketEngine,
        definition: &StudyDefinition,
        exchange_timestamp_unix_nanos: i64,
    ) -> Option<usize> {
        match definition.dependencies.first()? {
            StudyDependency::Market(input) => {
                let segments = engine.series_segments(&input.series)?;
                Some(segments.partition_point(|bar| {
                    bar.exchange_timestamp_unix_nanos < exchange_timestamp_unix_nanos
                }))
            }
            StudyDependency::Output(output) => Some(
                self.outputs
                    .get(output)?
                    .timeline
                    .lower_bound(exchange_timestamp_unix_nanos),
            ),
        }
    }

    fn primary_upper_bound(
        &self,
        engine: &MarketEngine,
        definition: &StudyDefinition,
        exchange_timestamp_unix_nanos: i64,
    ) -> Option<usize> {
        match definition.dependencies.first()? {
            StudyDependency::Market(input) => {
                let segments = engine.series_segments(&input.series)?;
                Some(segments.partition_point(|bar| {
                    bar.exchange_timestamp_unix_nanos <= exchange_timestamp_unix_nanos
                }))
            }
            StudyDependency::Output(output) => Some(
                self.outputs
                    .get(output)?
                    .timeline
                    .upper_bound(exchange_timestamp_unix_nanos),
            ),
        }
    }

    /// Re-executes every ready study transitively affected by one canonical
    /// market series, in registration/dependency order.
    ///
    /// This is currently the covering-recalculation path used for initial
    /// history hydration. Live incremental execution is intentionally kept
    /// separate until event-time dirty ranges are mapped across MTF inputs.
    #[cfg(test)]
    pub(crate) fn execute_ready_for_market(
        &mut self,
        engine: &MarketEngine,
        series: &BarSeriesKey,
    ) -> Result<Vec<StudyInstanceId>, StudyRuntimeError> {
        let mut no_live_market = |_input: &StudyMarketInput| None;
        let batch = self.execute_ready_for_market_with_live(engine, series, &mut no_live_market)?;
        if let Some(error) = batch.errors.into_iter().next() {
            Err(error)
        } else {
            Ok(batch.executed)
        }
    }

    pub(crate) fn execute_ready_for_market_with_live<'a, F>(
        &mut self,
        engine: &MarketEngine,
        series: &BarSeriesKey,
        live_market: &mut F,
    ) -> Result<StudyExecutionBatch, StudyRuntimeError>
    where
        F: FnMut(&StudyMarketInput) -> Option<StudyLiveMarketData<'a>>,
    {
        let mut affected = self
            .market_dependents
            .get(series)
            .cloned()
            .unwrap_or_default();
        for (study_id, node) in &self.studies {
            if !affected.contains(study_id) {
                continue;
            }
            for output_index in 0..node.definition.outputs.len() {
                if let Some(dependents) = self.output_dependents.get(&StudyOutputId {
                    study_id: *study_id,
                    output_index,
                }) {
                    affected.extend(dependents.iter().copied());
                }
            }
        }

        let mut batch = StudyExecutionBatch::default();
        let mut blocked = BTreeSet::new();
        for study_id in affected {
            if blocked.contains(&study_id) {
                continue;
            }
            match self.execute_ready_with_live(engine, study_id, live_market) {
                Ok(true) => batch.executed.push(study_id),
                Ok(false) | Err(StudyRuntimeError::StudyNotExecutable(_)) => {
                    blocked.extend(self.dependent_subtree(study_id)?);
                }
                Err(error) => {
                    batch.errors.push(error);
                    blocked.extend(self.dependent_subtree(study_id)?);
                }
            }
        }
        Ok(batch)
    }

    fn resolve_inputs(
        &self,
        engine: &MarketEngine,
        definition: &StudyDefinition,
    ) -> Option<Vec<ResolvedStudyInput>> {
        let mut inputs = Vec::with_capacity(definition.dependencies.len());
        for dependency in &definition.dependencies {
            match dependency {
                StudyDependency::Market(input) => {
                    let view = engine.series_view(&input.series)?;
                    inputs.push(ResolvedStudyInput::Market(StudyMarketSeries::new(view)));
                }
                StudyDependency::Output(output) => {
                    let series = self.outputs.get(output).cloned()?;
                    inputs.push(ResolvedStudyInput::Output(series));
                }
            }
        }
        Some(inputs)
    }

    fn resolve_live_inputs<'a, F>(
        definition: &StudyDefinition,
        live_market: &mut F,
    ) -> Option<Vec<Option<StudyLiveMarketData<'a>>>>
    where
        F: FnMut(&StudyMarketInput) -> Option<StudyLiveMarketData<'a>>,
    {
        let mut live_inputs = Vec::with_capacity(definition.dependencies.len());
        for dependency in &definition.dependencies {
            let StudyDependency::Market(input) = dependency else {
                live_inputs.push(None);
                continue;
            };
            let needs_live = input.streams.contains(MarketStream::Trades)
                || input.streams.contains(MarketStream::Quotes)
                || input.streams.contains(MarketStream::Depth);
            if needs_live {
                live_inputs.push(Some(live_market(input)?));
            } else {
                live_inputs.push(None);
            }
        }
        Some(live_inputs)
    }

    /// Reconciles all study market dependencies into `MarketEngine` data leases.
    ///
    /// One lease is held per distinct canonical series regardless of how many
    /// studies depend on it. Stream requirements are unioned before reaching the
    /// engine, so studies never allocate fake chart consumers or create a second
    /// provider-demand registry.
    ///
    /// Provider capability checks and capacity are preflighted before any lease
    /// mutation. A cached series, when present, remains readable directly from
    /// `MarketEngine::series_view`; this method does not duplicate covering bars.
    ///
    /// # Errors
    /// Returns an error when a requested stream set is unsupported, the engine's
    /// bounded lease capacity would be exceeded, or lease identity is exhausted.
    pub fn reconcile_market_leases(
        &mut self,
        engine: &mut MarketEngine,
    ) -> Result<Vec<StudyMarketLeaseChange>, StudyRuntimeError> {
        let desired = self.market_requirements();
        for (series, streams) in &desired {
            series
                .validate()
                .map_err(|_| StudyRuntimeError::InvalidMarketSeries)?;
            engine.verify_provider_stream_requirements(&series.provider_id, *streams)?;
        }

        let mut planned = BTreeMap::new();
        let mut changes = Vec::new();
        let mut reusable = Vec::new();
        let mut acquire = Vec::new();
        let mut updates = Vec::new();

        for (series, streams) in &desired {
            let Some(existing) = self.market_leases.get(series).copied() else {
                acquire.push((series.clone(), *streams));
                continue;
            };
            if engine.data_lease(existing.lease_id).is_none() {
                acquire.push((series.clone(), *streams));
                continue;
            }
            if existing.streams != *streams {
                updates.push((series.clone(), existing.lease_id, *streams));
            }
            planned.insert(
                series.clone(),
                StudyMarketLease {
                    lease_id: existing.lease_id,
                    streams: *streams,
                },
            );
        }

        for (series, lease) in &self.market_leases {
            if desired.contains_key(series) {
                continue;
            }
            if engine.data_lease(lease.lease_id).is_some() {
                reusable.push((series.clone(), *lease));
            }
        }

        let additional = acquire.len().saturating_sub(reusable.len());
        engine.preflight_data_lease_acquisitions(additional)?;

        for (series, lease_id, streams) in updates {
            engine.update_data_lease(lease_id, &series, streams)?;
            changes.push(StudyMarketLeaseChange {
                lease_id,
                series,
                streams,
                kind: StudyMarketLeaseChangeKind::Updated,
            });
        }

        for (series, streams) in acquire {
            if let Some((old_series, old_lease)) = reusable.pop() {
                engine.update_data_lease(old_lease.lease_id, &series, streams)?;
                changes.push(StudyMarketLeaseChange {
                    lease_id: old_lease.lease_id,
                    series: old_series,
                    streams: old_lease.streams,
                    kind: StudyMarketLeaseChangeKind::Released,
                });
                changes.push(StudyMarketLeaseChange {
                    lease_id: old_lease.lease_id,
                    series: series.clone(),
                    streams,
                    kind: StudyMarketLeaseChangeKind::Acquired,
                });
                planned.insert(
                    series,
                    StudyMarketLease {
                        lease_id: old_lease.lease_id,
                        streams,
                    },
                );
                continue;
            }
            let (lease_id, _) = engine.acquire_data_lease(&series, streams)?;
            changes.push(StudyMarketLeaseChange {
                lease_id,
                series: series.clone(),
                streams,
                kind: StudyMarketLeaseChangeKind::Acquired,
            });
            planned.insert(series, StudyMarketLease { lease_id, streams });
        }

        for (series, lease) in reusable {
            if engine.release_data_lease(lease.lease_id) {
                changes.push(StudyMarketLeaseChange {
                    lease_id: lease.lease_id,
                    series,
                    streams: lease.streams,
                    kind: StudyMarketLeaseChangeKind::Released,
                });
            }
        }

        self.market_leases = planned;
        Ok(changes)
    }

    /// Builds the deterministic recalculation plan for one canonical market-series mutation.
    ///
    /// # Errors
    /// Returns an error only when a trailing-window expansion overflows `usize`.
    pub fn plan_market_change(
        &self,
        series: &BarSeriesKey,
        range: StudyDirtyRange,
    ) -> Result<Vec<StudyCalculation>, StudyRuntimeError> {
        let mut dirty = BTreeMap::new();
        if let Some(dependents) = self.market_dependents.get(series) {
            for study_id in dependents {
                self.merge_invalidated_range(&mut dirty, *study_id, range)?;
            }
        }
        self.propagate_dirty(dirty)
    }

    /// Builds the downstream recalculation plan for one specific study output mutation.
    ///
    /// # Errors
    /// Returns an error for an unknown output or an overflowing trailing-window expansion.
    pub fn plan_output_change(
        &self,
        output: StudyOutputId,
        range: StudyDirtyRange,
    ) -> Result<Vec<StudyCalculation>, StudyRuntimeError> {
        self.validate_output(output)?;
        let mut dirty = BTreeMap::new();
        if let Some(dependents) = self.output_dependents.get(&output) {
            for study_id in dependents {
                self.merge_invalidated_range(&mut dirty, *study_id, range)?;
            }
        }
        self.propagate_dirty(dirty)
    }

    fn validate_definition(
        &self,
        owner: Option<ConsumerId>,
        definition: &StudyDefinition,
    ) -> Result<(), StudyRuntimeError> {
        let identifier = definition.identifier.trim();
        if identifier.is_empty() {
            return Err(StudyRuntimeError::InvalidIdentifier);
        }
        if identifier.len() > MAXIMUM_STUDY_IDENTIFIER_BYTES {
            return Err(StudyRuntimeError::IdentifierTooLong {
                maximum: MAXIMUM_STUDY_IDENTIFIER_BYTES,
            });
        }
        if definition.dependencies.is_empty() {
            return Err(StudyRuntimeError::MissingDependency);
        }
        if definition.dependencies.len() > self.config.maximum_dependencies_per_study.get() {
            return Err(StudyRuntimeError::TooManyDependencies {
                maximum: self.config.maximum_dependencies_per_study.get(),
            });
        }
        if definition.outputs.is_empty() {
            return Err(StudyRuntimeError::MissingOutput);
        }
        if definition.outputs.len() > self.config.maximum_outputs_per_study.get() {
            return Err(StudyRuntimeError::TooManyOutputs {
                maximum: self.config.maximum_outputs_per_study.get(),
            });
        }
        validate_study_outputs(&definition.outputs)?;
        validate_setting_specs(&definition.settings)?;

        for dependency in &definition.dependencies {
            match dependency {
                StudyDependency::Market(input) => {
                    input
                        .series
                        .validate()
                        .map_err(|_| StudyRuntimeError::InvalidMarketSeries)?;
                    if input.streams.is_empty() {
                        return Err(StudyRuntimeError::EmptyMarketStreams);
                    }
                }
                StudyDependency::Output(output) => {
                    self.validate_output(*output)?;
                    let dependency_owner = self
                        .studies
                        .get(&output.study_id)
                        .map(|node| node.owner)
                        .ok_or(StudyRuntimeError::UnknownOutput(*output))?;
                    if dependency_owner != owner {
                        return Err(StudyRuntimeError::CrossConsumerDependency {
                            study_id: StudyInstanceId(
                                NonZeroU64::new(self.next_id)
                                    .ok_or(StudyRuntimeError::IdentifierExhausted)?,
                            ),
                            dependency: *output,
                        });
                    }
                }
            }
        }
        Ok(())
    }

    fn validate_output(&self, output: StudyOutputId) -> Result<(), StudyRuntimeError> {
        let Some(node) = self.studies.get(&output.study_id) else {
            return Err(StudyRuntimeError::UnknownOutput(output));
        };
        if output.output_index >= node.definition.outputs.len() {
            return Err(StudyRuntimeError::UnknownOutput(output));
        }
        Ok(())
    }

    fn merge_invalidated_range(
        &self,
        dirty: &mut BTreeMap<StudyInstanceId, StudyDirtyRange>,
        study_id: StudyInstanceId,
        source_range: StudyDirtyRange,
    ) -> Result<(), StudyRuntimeError> {
        let node = self
            .studies
            .get(&study_id)
            .ok_or(StudyRuntimeError::UnknownStudy(study_id))?;
        let invalidated = source_range.invalidated_by(node.definition.invalidation)?;
        dirty
            .entry(study_id)
            .and_modify(|range| *range = range.merge(invalidated))
            .or_insert(invalidated);
        Ok(())
    }

    fn propagate_dirty(
        &self,
        mut dirty: BTreeMap<StudyInstanceId, StudyDirtyRange>,
    ) -> Result<Vec<StudyCalculation>, StudyRuntimeError> {
        for (study_id, node) in &self.studies {
            let Some(source_range) = dirty.get(study_id).copied() else {
                continue;
            };
            for output_index in 0..node.definition.outputs.len() {
                let output = StudyOutputId {
                    study_id: *study_id,
                    output_index,
                };
                let Some(dependents) = self.output_dependents.get(&output) else {
                    continue;
                };
                for dependent in dependents {
                    self.merge_invalidated_range(&mut dirty, *dependent, source_range)?;
                }
            }
        }

        Ok(dirty
            .into_iter()
            .map(|(study_id, range)| StudyCalculation { study_id, range })
            .collect())
    }

    fn rebuild_dependency_indexes(&mut self) {
        self.market_dependents.clear();
        self.output_dependents.clear();
        for (study_id, node) in &self.studies {
            for dependency in &node.definition.dependencies {
                match dependency {
                    StudyDependency::Market(input) => {
                        self.market_dependents
                            .entry(input.series.clone())
                            .or_default()
                            .insert(*study_id);
                    }
                    StudyDependency::Output(output) => {
                        self.output_dependents
                            .entry(*output)
                            .or_default()
                            .insert(*study_id);
                    }
                }
            }
        }
    }

    fn remove_outputs_for_study(&mut self, study_id: StudyInstanceId) {
        let removed_points = self
            .outputs
            .iter()
            .filter(|(output, _)| output.study_id == study_id)
            .map(|(_, series)| series.len())
            .sum::<usize>();
        self.outputs.retain(|output, _| output.study_id != study_id);
        self.output_points = self.output_points.saturating_sub(removed_points);
    }

    fn remove_study_artifacts(&mut self, study_id: StudyInstanceId) {
        self.style_overlays.remove(&study_id);
        self.scenes.remove(&study_id);
        self.scene_deltas.remove(&study_id);
        self.alert_ledgers.remove(&study_id);
        self.pending_alerts.remove(&study_id);
    }
}

fn primary_timeline(input: &ResolvedStudyInput) -> StudyOutputTimeline {
    match input {
        ResolvedStudyInput::Market(series) => StudyOutputTimeline::market(series.view.clone()),
        ResolvedStudyInput::Output(series) => series.timeline.clone(),
    }
}

fn clamp_dirty_range(range: StudyDirtyRange, length: usize) -> Option<StudyDirtyRange> {
    if range.start >= length {
        return None;
    }
    let end = range.end_exclusive.unwrap_or(length).min(length);
    (range.start < end).then_some(StudyDirtyRange {
        start: range.start,
        end_exclusive: Some(end),
    })
}

fn bounded_execution_detail(mut detail: String) -> String {
    if detail.len() <= MAXIMUM_STUDY_EXECUTION_ERROR_BYTES {
        return detail;
    }
    let mut end = MAXIMUM_STUDY_EXECUTION_ERROR_BYTES;
    while !detail.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    detail.truncate(end);
    detail
}

#[cfg(test)]
mod tests;
