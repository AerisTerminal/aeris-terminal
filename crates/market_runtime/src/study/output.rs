use super::{
    NativeStudyState, StudyDecimal, StudyDirtyRange, StudyLiveMarketData, StudyMarketInput,
    StudyMarketSeries, StudyOutputId, StudySettingSpec, StudySettings,
};
use aeris_market_engine::SeriesView;
use std::{
    array, fmt,
    num::NonZeroUsize,
    sync::{Arc, OnceLock},
};

/// One static dependency declared before a study begins calculating.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StudyDependency {
    Market(StudyMarketInput),
    Output(StudyOutputId),
}

/// How an input mutation can affect this study's output rows.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StudyInvalidationPolicy {
    /// Only output rows aligned with the changed input range can change.
    SameRange,
    /// A change can affect every later output row, as with recursive smoothing.
    FromFirstChanged,
    /// A changed input row can affect this many trailing output rows in total.
    TrailingWindow { bars: NonZeroUsize },
}

/// Scalar plot family requested by one study output.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StudyPlotKind {
    Line,
    Histogram,
    Area,
}

/// Semantic pane placement. Outputs using the same dedicated group share one
/// chart pane; actual Aeris Charts pane identities remain presentation-owned.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StudyPaneTarget {
    Price,
    Dedicated { group: u8 },
}

/// Semantic scale placement inside the selected pane.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StudyScaleTarget {
    Primary,
    Left,
    Overlay,
}

/// One fixed-value background channel requested behind a scalar output.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StudyThresholdRegion {
    pub lower: StudyDecimal,
    pub upper: StudyDecimal,
}

/// Semantic per-point styling policy for one scalar output.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum StudyPointStyle {
    /// One ordinary series color for every row.
    #[default]
    Uniform,
    /// Four-state histogram coloring based on sign and movement toward/away from zero.
    MomentumHistogram,
}

/// Declarative presentation metadata for one scalar study output.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StudyOutputSpec {
    pub identifier: String,
    pub title: String,
    /// Optional short label used when a host groups multiple outputs into one legend row.
    /// `None` leaves the value unlabeled, which is useful for visually grouped families such as
    /// band envelopes.
    pub legend_label: Option<String>,
    pub plot: StudyPlotKind,
    pub pane: StudyPaneTarget,
    pub scale: StudyScaleTarget,
    pub threshold_region: Option<StudyThresholdRegion>,
    pub point_style: StudyPointStyle,
}

/// Runtime-independent definition for one native study instance.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StudyDefinition {
    pub identifier: String,
    pub dependencies: Vec<StudyDependency>,
    pub settings: Vec<StudySettingSpec>,
    pub outputs: Vec<StudyOutputSpec>,
    pub invalidation: StudyInvalidationPolicy,
}

/// Explicit finite limits for one study runtime.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StudyRuntimeConfig {
    pub maximum_studies: NonZeroUsize,
    pub maximum_dependencies_per_study: NonZeroUsize,
    pub maximum_outputs_per_study: NonZeroUsize,
    pub maximum_points_per_output: NonZeroUsize,
    pub maximum_total_output_points: NonZeroUsize,
    pub maximum_state_bytes_per_study: NonZeroUsize,
    pub maximum_total_state_bytes: NonZeroUsize,
}

const STUDY_OUTPUT_VALUE_LEAF_LEN: usize = 64;
const STUDY_OUTPUT_VALUE_BRANCH_FACTOR: usize = 32;

#[derive(Clone)]
pub(super) struct StudyOutputTimeline(pub(super) Arc<StudyOutputTimelineInner>);

pub(super) struct StudyOutputTimelineInner {
    pub(super) view: SeriesView,
    pub(super) materialized: OnceLock<Arc<[i64]>>,
}

impl StudyOutputTimeline {
    pub(super) fn market(view: SeriesView) -> Self {
        Self(Arc::new(StudyOutputTimelineInner {
            view,
            materialized: OnceLock::new(),
        }))
    }

    pub(super) fn len(&self) -> usize {
        self.0.view.len()
    }

    pub(super) fn timestamp(&self, index: usize) -> Option<i64> {
        self.0
            .view
            .get(index)
            .map(|bar| bar.exchange_timestamp_unix_nanos)
    }

    pub(super) fn lower_bound(&self, timestamp_unix_nanos: i64) -> usize {
        self.0
            .view
            .partition_point(|bar| bar.exchange_timestamp_unix_nanos < timestamp_unix_nanos)
    }

    pub(super) fn upper_bound(&self, timestamp_unix_nanos: i64) -> usize {
        self.0
            .view
            .partition_point(|bar| bar.exchange_timestamp_unix_nanos <= timestamp_unix_nanos)
    }

    pub(super) fn timestamps(&self) -> &[i64] {
        self.0
            .materialized
            .get_or_init(|| {
                self.0
                    .view
                    .iter()
                    .map(|bar| bar.exchange_timestamp_unix_nanos)
                    .collect::<Vec<_>>()
                    .into()
            })
            .as_ref()
    }
}

enum StudyOutputValueNode {
    Branch(Box<[Option<Arc<StudyOutputValueNode>>; STUDY_OUTPUT_VALUE_BRANCH_FACTOR]>),
    Leaf(Box<[Option<f64>; STUDY_OUTPUT_VALUE_LEAF_LEN]>),
}

#[derive(Clone)]
pub(super) struct StudyOutputValues {
    root: Option<Arc<StudyOutputValueNode>>,
    depth: u8,
    len: usize,
}

impl StudyOutputValues {
    fn empty(len: usize) -> Self {
        Self {
            root: None,
            depth: 0,
            len,
        }
    }

    fn from_dense(values: &[Option<f64>]) -> Self {
        let len = values.len();
        let mut output = Self::empty(len);
        for (chunk_index, chunk) in values.chunks(STUDY_OUTPUT_VALUE_LEAF_LEN).enumerate() {
            if chunk.iter().all(Option::is_none) {
                continue;
            }
            let mut leaf = [None; STUDY_OUTPUT_VALUE_LEAF_LEN];
            leaf[..chunk.len()].copy_from_slice(chunk);
            output.insert_leaf(chunk_index, Box::new(leaf));
        }
        output
    }

    fn value_at(&self, index: usize) -> Option<f64> {
        debug_assert!(index < self.len);
        let chunk_index = index / STUDY_OUTPUT_VALUE_LEAF_LEN;
        let offset = index % STUDY_OUTPUT_VALUE_LEAF_LEN;
        let mut node = self.root.as_ref()?;
        let mut depth = self.depth;
        let mut remaining = chunk_index;
        while depth > 0 {
            let StudyOutputValueNode::Branch(children) = node.as_ref() else {
                return None;
            };
            let child_capacity = Self::chunk_capacity(depth - 1);
            let child_index = remaining / child_capacity;
            remaining %= child_capacity;
            node = children.get(child_index).and_then(Option::as_ref)?;
            depth -= 1;
        }
        let StudyOutputValueNode::Leaf(values) = node.as_ref() else {
            return None;
        };
        values[offset]
    }

    fn set(&mut self, index: usize, value: Option<f64>) -> Result<(), String> {
        if index >= self.len {
            return Err("study output row is outside the calculation range".to_string());
        }
        let chunk_index = index / STUDY_OUTPUT_VALUE_LEAF_LEN;
        self.ensure_depth(chunk_index);
        self.root = Some(Self::updated_value(
            self.root.as_ref(),
            self.depth,
            chunk_index,
            index % STUDY_OUTPUT_VALUE_LEAF_LEN,
            value,
        ));
        Ok(())
    }

    fn resized(mut self, len: usize) -> Self {
        self.len = len;
        self
    }

    fn to_vec(&self) -> Vec<Option<f64>> {
        (0..self.len).map(|index| self.value_at(index)).collect()
    }

    fn chunk_capacity(depth: u8) -> usize {
        let mut capacity = 1usize;
        for _ in 0..depth {
            capacity = capacity.saturating_mul(STUDY_OUTPUT_VALUE_BRANCH_FACTOR);
        }
        capacity
    }

    fn ensure_depth(&mut self, chunk_index: usize) {
        while chunk_index >= Self::chunk_capacity(self.depth) {
            let mut children = array::from_fn(|_| None);
            children[0] = self.root.take();
            self.root = Some(Arc::new(StudyOutputValueNode::Branch(Box::new(children))));
            self.depth = self.depth.saturating_add(1);
        }
    }

    fn insert_leaf(
        &mut self,
        chunk_index: usize,
        values: Box<[Option<f64>; STUDY_OUTPUT_VALUE_LEAF_LEN]>,
    ) {
        self.ensure_depth(chunk_index);
        self.root = Some(Self::updated_leaf(
            self.root.as_ref(),
            self.depth,
            chunk_index,
            Arc::new(StudyOutputValueNode::Leaf(values)),
        ));
    }

    fn updated_leaf(
        node: Option<&Arc<StudyOutputValueNode>>,
        depth: u8,
        chunk_index: usize,
        leaf: Arc<StudyOutputValueNode>,
    ) -> Arc<StudyOutputValueNode> {
        if depth == 0 {
            return leaf;
        }
        let child_capacity = Self::chunk_capacity(depth - 1);
        let child_index = chunk_index / child_capacity;
        let remainder = chunk_index % child_capacity;
        let mut children = match node.map(Arc::as_ref) {
            Some(StudyOutputValueNode::Branch(children)) => (**children).clone(),
            _ => array::from_fn(|_| None),
        };
        children[child_index] = Some(Self::updated_leaf(
            children[child_index].as_ref(),
            depth - 1,
            remainder,
            leaf,
        ));
        Arc::new(StudyOutputValueNode::Branch(Box::new(children)))
    }

    fn updated_value(
        node: Option<&Arc<StudyOutputValueNode>>,
        depth: u8,
        chunk_index: usize,
        offset: usize,
        value: Option<f64>,
    ) -> Arc<StudyOutputValueNode> {
        if depth == 0 {
            let mut values = match node.map(Arc::as_ref) {
                Some(StudyOutputValueNode::Leaf(values)) => **values,
                _ => [None; STUDY_OUTPUT_VALUE_LEAF_LEN],
            };
            values[offset] = value;
            return Arc::new(StudyOutputValueNode::Leaf(Box::new(values)));
        }
        let child_capacity = Self::chunk_capacity(depth - 1);
        let child_index = chunk_index / child_capacity;
        let remainder = chunk_index % child_capacity;
        let mut children = match node.map(Arc::as_ref) {
            Some(StudyOutputValueNode::Branch(children)) => (**children).clone(),
            _ => array::from_fn(|_| None),
        };
        children[child_index] = Some(Self::updated_value(
            children[child_index].as_ref(),
            depth - 1,
            remainder,
            offset,
            value,
        ));
        Arc::new(StudyOutputValueNode::Branch(Box::new(children)))
    }
}

/// Immutable scalar output series produced by one native study output.
///
/// Timestamps are inherited from the study's first dependency and shared by all
/// outputs from the same calculation. `None` represents a deliberate gap such
/// as an indicator warm-up period. Incremental generations structurally share
/// unchanged value blocks; full contiguous slices are materialized only when a
/// publication consumer explicitly requests them.
#[derive(Clone)]
pub struct StudyOutputSeries {
    pub(super) timeline: StudyOutputTimeline,
    pub(super) values: StudyOutputValues,
    pub(super) materialized_values: Arc<OnceLock<Arc<[Option<f64>]>>>,
    pub(super) generation: u64,
}

impl fmt::Debug for StudyOutputSeries {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StudyOutputSeries")
            .field("len", &self.len())
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

impl PartialEq for StudyOutputSeries {
    fn eq(&self, other: &Self) -> bool {
        self.generation == other.generation
            && self.timestamps() == other.timestamps()
            && self.values() == other.values()
    }
}

impl StudyOutputSeries {
    /// Returns the number of output rows.
    #[must_use]
    pub fn len(&self) -> usize {
        self.values.len
    }

    /// Returns whether no output rows are available.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.values.len == 0
    }

    /// Returns the output generation committed by the runtime.
    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    /// Returns the exact row timestamp.
    #[must_use]
    pub fn timestamp_unix_nanos(&self, index: usize) -> Option<i64> {
        self.timeline.timestamp(index)
    }

    /// Returns one finite scalar value or a deliberate gap.
    #[must_use]
    pub fn value(&self, index: usize) -> Option<Option<f64>> {
        (index < self.len()).then(|| self.values.value_at(index))
    }

    /// Returns all timestamps for binary-search alignment by secondary inputs.
    #[must_use]
    pub fn timestamps(&self) -> &[i64] {
        self.timeline.timestamps()
    }

    /// Returns all output values.
    #[must_use]
    pub fn values(&self) -> &[Option<f64>] {
        self.materialized_values
            .get_or_init(|| self.values.to_vec().into())
            .as_ref()
    }
}

#[derive(Clone)]
pub(super) enum ResolvedStudyInput {
    Market(StudyMarketSeries),
    Output(StudyOutputSeries),
}

/// One read-only input exposed to native Rust study code.
#[derive(Clone, Copy)]
pub enum StudyInputSeries<'a> {
    Market(&'a StudyMarketSeries),
    Output(&'a StudyOutputSeries),
}

/// One bounded output buffer for the current native calculation.
pub struct StudyOutputBuffer {
    values: StudyOutputBufferValues,
}

enum StudyOutputBufferValues {
    Dense(Vec<Option<f64>>),
    Persistent(StudyOutputValues),
}

impl StudyOutputBuffer {
    fn new(len: usize) -> Self {
        Self {
            values: StudyOutputBufferValues::Dense(vec![None; len]),
        }
    }

    pub(super) fn from_previous(
        timeline: &StudyOutputTimeline,
        previous: Option<&StudyOutputSeries>,
        dirty: StudyDirtyRange,
    ) -> (Self, usize) {
        let Some(previous) = previous else {
            return (Self::new(timeline.len()), timeline.len());
        };
        if Self::can_reuse_previous(timeline, previous, dirty) {
            let mut output = Self {
                values: StudyOutputBufferValues::Persistent(
                    previous.values.clone().resized(timeline.len()),
                ),
            };
            let cleared = output.clear(dirty);
            return (output, cleared);
        }

        let mut output = Self::new(timeline.len());
        let mut previous_index = 0;
        let mut current_index = 0;
        while previous_index < previous.len() && current_index < timeline.len() {
            let previous_timestamp = previous
                .timestamp_unix_nanos(previous_index)
                .expect("bounded previous output timestamp");
            let current_timestamp = timeline
                .timestamp(current_index)
                .expect("bounded current output timestamp");
            match previous_timestamp.cmp(&current_timestamp) {
                std::cmp::Ordering::Less => previous_index += 1,
                std::cmp::Ordering::Greater => current_index += 1,
                std::cmp::Ordering::Equal => {
                    if let StudyOutputBufferValues::Dense(values) = &mut output.values {
                        values[current_index] = previous.value(previous_index).flatten();
                    }
                    previous_index += 1;
                    current_index += 1;
                }
            }
        }
        let scanned = previous_index.saturating_add(current_index);
        output.clear(dirty);
        (output, scanned)
    }

    fn can_reuse_previous(
        timeline: &StudyOutputTimeline,
        previous: &StudyOutputSeries,
        dirty: StudyDirtyRange,
    ) -> bool {
        let previous_len = previous.len();
        let current_len = timeline.len();
        if dirty.start == 0
            || previous_len == 0
            || current_len < previous_len
            || dirty.start > previous_len
        {
            return false;
        }
        if timeline.timestamp(0) != previous.timestamp_unix_nanos(0)
            || timeline.timestamp(dirty.start - 1) != previous.timestamp_unix_nanos(dirty.start - 1)
        {
            return false;
        }
        if dirty.start < previous_len
            && timeline.timestamp(dirty.start) != previous.timestamp_unix_nanos(dirty.start)
        {
            return false;
        }
        current_len == previous_len
            || timeline.timestamp(previous_len - 1)
                == previous.timestamp_unix_nanos(previous_len - 1)
    }

    fn clear(&mut self, range: StudyDirtyRange) -> usize {
        let end = range.end_exclusive.unwrap_or(self.len()).min(self.len());
        if range.start >= end {
            return 0;
        }
        match &mut self.values {
            StudyOutputBufferValues::Dense(values) => values[range.start..end].fill(None),
            StudyOutputBufferValues::Persistent(values) => {
                for index in range.start..end {
                    values.set(index, None).expect("bounded output clear range");
                }
            }
        }
        end - range.start
    }

    /// Returns the output row count inherited from the first dependency.
    #[must_use]
    pub fn len(&self) -> usize {
        match &self.values {
            StudyOutputBufferValues::Dense(values) => values.len(),
            StudyOutputBufferValues::Persistent(values) => values.len,
        }
    }

    /// Returns whether this output has no rows.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Writes one finite scalar value or an explicit gap.
    ///
    /// # Errors
    /// Returns an error for an out-of-range row or non-finite value.
    pub fn set(&mut self, index: usize, value: Option<f64>) -> Result<(), String> {
        if value.is_some_and(|value| !value.is_finite()) {
            return Err("study output must be finite".to_string());
        }
        match &mut self.values {
            StudyOutputBufferValues::Dense(values) => {
                let slot = values.get_mut(index).ok_or_else(|| {
                    "study output row is outside the calculation range".to_string()
                })?;
                *slot = value;
                Ok(())
            }
            StudyOutputBufferValues::Persistent(values) => values.set(index, value),
        }
    }

    pub(super) fn into_values(self) -> StudyOutputValues {
        match self.values {
            StudyOutputBufferValues::Dense(values) => StudyOutputValues::from_dense(&values),
            StudyOutputBufferValues::Persistent(values) => values,
        }
    }
}

/// In-process execution context for one trusted native Rust study calculation.
///
/// Inputs are immutable snapshots. Outputs are bounded buffers owned by the
/// runtime. Studies receive timestamps rather than pixel coordinates and cannot
/// reach provider sessions, `MarketEngine`, Aeris Charts, GPUI, or GPU state.
pub struct StudyExecutionContext<'a> {
    pub(super) settings: &'a StudySettings,
    pub(super) inputs: &'a [ResolvedStudyInput],
    pub(super) live_inputs: &'a [Option<StudyLiveMarketData<'a>>],
    pub(super) timeline: &'a StudyOutputTimeline,
    pub(super) dirty: StudyDirtyRange,
    pub(super) outputs: &'a mut [StudyOutputBuffer],
    pub(super) state: Option<&'a mut NativeStudyState>,
}

/// Immutable half of one native study calculation.
#[derive(Clone, Copy)]
pub struct StudyExecutionInputs<'a> {
    settings: &'a StudySettings,
    inputs: &'a [ResolvedStudyInput],
    live_inputs: &'a [Option<StudyLiveMarketData<'a>>],
    timeline: &'a StudyOutputTimeline,
    dirty: StudyDirtyRange,
}

impl<'a> StudyExecutionInputs<'a> {
    /// Returns validated typed settings.
    #[must_use]
    pub const fn settings(self) -> &'a StudySettings {
        self.settings
    }

    /// Returns the primary output timeline inherited from dependency zero.
    #[must_use]
    pub fn timestamps(self) -> &'a [i64] {
        self.timeline.timestamps()
    }

    /// Returns the output rows that must be recomputed for this invocation.
    #[must_use]
    pub const fn dirty_range(self) -> StudyDirtyRange {
        self.dirty
    }

    /// Returns one declared input in definition order.
    #[must_use]
    pub fn input(self, index: usize) -> Option<StudyInputSeries<'a>> {
        self.inputs.get(index).map(|input| match input {
            ResolvedStudyInput::Market(series) => StudyInputSeries::Market(series),
            ResolvedStudyInput::Output(series) => StudyInputSeries::Output(series),
        })
    }

    /// Returns borrowed quote/trade/depth state for one market dependency.
    ///
    /// Output dependencies and market dependencies that requested only bars
    /// return `None`. The returned views borrow the coordinator's existing
    /// canonical live state and never allocate a second depth/trade snapshot.
    #[must_use]
    pub fn live_market(self, index: usize) -> Option<StudyLiveMarketData<'a>> {
        self.live_inputs.get(index).copied().flatten()
    }
}

impl StudyExecutionContext<'_> {
    /// Returns validated typed settings.
    #[must_use]
    pub const fn settings(&self) -> &StudySettings {
        self.settings
    }

    /// Returns the primary output timeline inherited from dependency zero.
    #[must_use]
    pub fn timestamps(&self) -> &[i64] {
        self.timeline.timestamps()
    }

    /// Returns the output rows that must be recomputed for this invocation.
    #[must_use]
    pub const fn dirty_range(&self) -> StudyDirtyRange {
        self.dirty
    }

    /// Returns one declared input in definition order.
    #[must_use]
    pub fn input(&self, index: usize) -> Option<StudyInputSeries<'_>> {
        self.inputs.get(index).map(|input| match input {
            ResolvedStudyInput::Market(series) => StudyInputSeries::Market(series),
            ResolvedStudyInput::Output(series) => StudyInputSeries::Output(series),
        })
    }

    /// Returns borrowed quote/trade/depth state for one market dependency.
    #[must_use]
    pub fn live_market(&self, index: usize) -> Option<StudyLiveMarketData<'_>> {
        self.live_inputs.get(index).copied().flatten()
    }

    /// Returns one mutable numeric output buffer.
    #[must_use]
    pub fn output(&mut self, index: usize) -> Option<&mut StudyOutputBuffer> {
        self.outputs.get_mut(index)
    }

    /// Returns the runtime-owned mutable state value for this study when its
    /// native registration declared state of type `T`.
    ///
    /// The runtime calculates against a cloned candidate and commits that state
    /// only together with successful outputs, so rejected or panicking native
    /// executions cannot partially advance recursive checkpoints.
    #[must_use]
    pub fn state_mut<T: 'static>(&mut self) -> Option<&mut T> {
        self.state.as_deref_mut()?.value_mut::<T>()
    }

    /// Splits immutable inputs/settings from mutable output buffers so native
    /// study code can read several inputs while writing several outputs without
    /// cloning canonical market data.
    #[must_use]
    pub fn split(&mut self) -> (StudyExecutionInputs<'_>, &mut [StudyOutputBuffer]) {
        (
            StudyExecutionInputs {
                settings: self.settings,
                inputs: self.inputs,
                live_inputs: self.live_inputs,
                timeline: self.timeline,
                dirty: self.dirty,
            },
            self.outputs,
        )
    }

    /// Splits immutable inputs/settings from one typed runtime state value and
    /// all mutable output buffers.
    ///
    /// This is the stateful counterpart to [`Self::split`]. `None` means the
    /// registration has no state or registered a different concrete state type.
    #[must_use]
    pub fn split_with_state<T: 'static>(
        &mut self,
    ) -> Option<(StudyExecutionInputs<'_>, &mut T, &mut [StudyOutputBuffer])> {
        let state = self.state.as_deref_mut()?.value_mut::<T>()?;
        Some((
            StudyExecutionInputs {
                settings: self.settings,
                inputs: self.inputs,
                live_inputs: self.live_inputs,
                timeline: self.timeline,
                dirty: self.dirty,
            },
            state,
            self.outputs,
        ))
    }
}
