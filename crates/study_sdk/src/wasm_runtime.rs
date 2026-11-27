//! Phase L WebAssembly Sandbox Runtime (`wasmi` 2.x).
//!
//! Compiles, validates, and executes user-authored `.wasm` study artifacts against
//! `STUDY_SANDBOX_ABI_VERSION = 1` with:
//! - Zero host imports (no WASI, filesystem, network, clock, or environment authority).
//! - Deterministic instruction fuel metering (`Config::consume_fuel(true)`).
//! - Strict linear memory and table ceilings (`StoreLimitsBuilder`).
//! - Direct `NativeStudyProgram` bridge so admitted `.wasm` studies run inside
//!   `StudyRuntime` with full transactional rollback and generation fencing.

use crate::sandbox::{
    MAXIMUM_SANDBOX_ARTIFACT_BYTES, STUDY_SANDBOX_ABI_VERSION, StudySandboxColumnarBars,
    StudySandboxLimits,
};
use crate::{
    AggressorSide, BarPeriod, MAXIMUM_STUDY_ALERT_MESSAGE_BYTES, MAXIMUM_STUDY_ALERTS_PER_WAVE,
    MAXIMUM_STUDY_IDENTIFIER_BYTES, MAXIMUM_STUDY_POLYLINE_POINTS, MAXIMUM_STUDY_SCENE_OBJECTS,
    MAXIMUM_STUDY_SCENE_TEXT_BYTES, MAXIMUM_STUDY_TABLE_CELLS, NativeStudyProgram,
    StudyAlertEvent, StudyAlertFrequency, StudyBoxObject, StudyColor, StudyExecutionContext,
    StudyFillSpec, StudyInputSeries, StudyLabelObject, StudyLabelStyle, StudyLevelSpec,
    StudyLineExtend, StudyLineFillObject, StudyLineObject, StudyLineStyle, StudyMtfConfirmation,
    StudyMtfGapPolicy, StudyPoint, StudyPolylineObject, StudyRequestContext, StudySceneObject,
    StudySceneState, StudySettingValue, StudySource, StudyTableCell, StudyTableObject,
    StudyTablePosition, StudyTextAlign, align_higher_timeframe_bar, fixed_point_to_f64,
};
use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use wasmi::{Config, Engine, Linker, Module, Store, StoreLimits, StoreLimitsBuilder, TrapCode};

/// Required guest export returning the supported sandbox ABI version (`u32`).
pub const WASM_EXPORT_ABI_VERSION: &str = "aeris_study_abi_version";
/// Required guest export allocating `byte_len` bytes in guest linear memory (`u32 -> u32`).
pub const WASM_EXPORT_ALLOC: &str = "aeris_study_alloc";
/// Required guest export computing one output series over columnized OHLCV `f64` bars:
/// `(bars_ptr: u32, bar_count: u32, out_ptr: u32) -> i32`.
pub const WASM_EXPORT_CALCULATE: &str = "aeris_study_calculate";
/// Optional guest export computing `output_count` contiguous output series over columnized
/// OHLCV `f64` bars: `(bars_ptr: u32, bar_count: u32, out_ptr: u32, output_count: u32) -> i32`.
pub const WASM_EXPORT_CALCULATE_MULTI: &str = "aeris_study_calculate_multi";
/// Optional guest export setting one numeric input parameter `(index: u32, value: f64)` before
/// calculation.
pub const WASM_EXPORT_SET_PARAM: &str = "aeris_study_set_param";
/// Optional guest export setting one UTF-8 string input parameter `(index: u32, ptr: u32, len: u32)`.
pub const WASM_EXPORT_SET_STR_PARAM: &str = "aeris_study_set_str_param";
/// Optional guest export setting the chart's visible bar index range `(left: u32, right: u32)`.
pub const WASM_EXPORT_SET_VIEWPORT: &str = "aeris_study_set_viewport";
/// Optional guest export receiving the bar timestamps slice `(ptr: u32, count: u32)` where
/// `ptr` points to `count` little-endian `i64` Unix-nanosecond timestamps.
pub const WASM_EXPORT_SET_TIMESTAMPS: &str = "aeris_study_set_timestamps";
/// Optional guest export receiving instrument & execution metadata:
/// `(timeframe_secs: u32, price_scale: u32, qty_scale: u32, flags: u32, symbol_ptr: u32, symbol_len: u32)`.
pub const WASM_EXPORT_SET_META: &str = "aeris_study_set_meta";
/// Optional guest export receiving one aligned secondary (MTF or multi-symbol) OHLCV series:
/// `(slot: u32, ptr: u32, bar_count: u32)` where `ptr` points to 5 contiguous `f64` columns.
pub const WASM_EXPORT_SET_SECONDARY: &str = "aeris_study_set_secondary";
/// Optional guest export receiving per-bar order-flow columns (`buy_vol`, `sell_vol`, `delta`,
/// `trade_count`, `poc`): `(ptr: u32, bar_count: u32)`.
pub const WASM_EXPORT_SET_ORDER_FLOW: &str = "aeris_study_set_order_flow";
/// Optional guest export returning the linear-memory pointer of the encoded scene command buffer.
pub const WASM_EXPORT_SCENE_PTR: &str = "aeris_study_scene_ptr";
/// Optional guest export returning the byte length of the encoded scene command buffer.
pub const WASM_EXPORT_SCENE_LEN: &str = "aeris_study_scene_len";
/// Optional guest export computing one incremental bar step inside a persistent bounded store:
/// `(bar_index: u32, flags: u32, open: f64, high: f64, low: f64, close: f64, volume: f64, timestamp_ns: i64, out_ptr: u32, output_count: u32) -> i32`.
pub const WASM_EXPORT_ON_BAR: &str = "aeris_study_on_bar";
/// Required guest linear memory export name.
pub const WASM_EXPORT_MEMORY: &str = "memory";

/// Maximum encoded scene command buffer size read from one sandbox execution (512 KiB).
const MAX_WASM_SCENE_BYTES: usize = 512 * 1024;
/// Maximum runtime log entries decoded from one sandbox execution wave.
pub const MAX_WASM_LOGS_PER_WAVE: usize = 32;
/// Maximum UTF-8 bytes retained per runtime log entry.
pub const MAX_WASM_LOG_MESSAGE_BYTES: usize = 256;
/// Maximum UTF-8 bytes passed for the instrument symbol metadata.
const MAX_WASM_SYMBOL_BYTES: usize = 64;

const OP_BOX: u8 = 1;
const OP_LINE: u8 = 2;
const OP_POLYLINE: u8 = 3;
const OP_LABEL: u8 = 4;
const OP_TABLE_CELL: u8 = 5;
const OP_BAR_COLOR: u8 = 6;
const OP_BG_COLOR: u8 = 7;
const OP_ALERT: u8 = 8;
const OP_LOG: u8 = 9;
const OP_BOX_EX: u8 = 10;
const OP_LINE_FILL: u8 = 11;
const OP_PLOT_COLOR: u8 = 12;
const OP_PLOT_SHAPE: u8 = 13;
const OP_STRATEGY_ENTRY: u8 = 14;
const OP_STRATEGY_EXIT: u8 = 15;
const OP_STRATEGY_CLOSE: u8 = 16;
const OP_STRATEGY_CLOSE_ALL: u8 = 17;

/// Maximum numeric/string input parameters passed across the WASM boundary.
const MAX_WASM_PARAMS: usize = 32;
/// Maximum UTF-8 bytes passed for a single string parameter.
const MAX_WASM_STR_PARAM_BYTES: usize = 256;
/// Maximum aligned secondary (MTF / multi-symbol) series passed to one WASM study.
pub const MAX_WASM_SECONDARY_SERIES: usize = 4;

const META_FLAG_REALTIME: u32 = 1 << 0;
const META_FLAG_CONFIRMED_BAR: u32 = 1 << 1;
const META_FLAG_NEW_BAR: u32 = 1 << 2;

/// Number of `f64` columns packed into the input buffer (`open`, `high`, `low`, `close`, `volume`).
const INPUT_COLUMN_COUNT: usize = 5;

/// Structured failure returned when compiling, validating, or executing a sandboxed `.wasm` study.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WasmSandboxError {
    EmptyArtifact,
    ArtifactTooLarge { maximum: usize, actual: usize },
    CompileFailed(String),
    ImportDenied { module: String, name: String },
    InstantiationFailed(String),
    MissingExport(&'static str),
    AbiVersionMismatch { expected: u32, actual: u32 },
    InconsistentInputBars,
    InputTooLarge { maximum: usize, actual: usize },
    AllocationFailed { requested_bytes: usize },
    MemoryAccessOutOfBounds,
    FuelExhausted,
    MemoryLimitExceeded,
    GuestTrap(String),
    GuestError(i32),
    InvalidOutputValue { index: usize },
}

impl fmt::Display for WasmSandboxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyArtifact => write!(f, "sandbox wasm artifact is empty"),
            Self::ArtifactTooLarge { maximum, actual } => {
                write!(
                    f,
                    "sandbox wasm artifact size {actual} exceeds {maximum} bytes"
                )
            }
            Self::CompileFailed(msg) => write!(f, "sandbox wasm compilation failed: {msg}"),
            Self::ImportDenied { module, name } => {
                write!(
                    f,
                    "sandbox wasm module imports disallowed host symbol {module}.{name}"
                )
            }
            Self::InstantiationFailed(msg) => {
                write!(f, "sandbox wasm instantiation failed: {msg}")
            }
            Self::MissingExport(name) => {
                write!(f, "sandbox wasm module is missing required export '{name}'")
            }
            Self::AbiVersionMismatch { expected, actual } => {
                write!(
                    f,
                    "sandbox wasm ABI version {actual} does not match host ABI version {expected}"
                )
            }
            Self::InconsistentInputBars => {
                write!(
                    f,
                    "columnar bar input lengths or timestamps are inconsistent"
                )
            }
            Self::InputTooLarge { maximum, actual } => {
                write!(
                    f,
                    "input bar count {actual} exceeds sandbox limit {maximum}"
                )
            }
            Self::AllocationFailed { requested_bytes } => {
                write!(
                    f,
                    "guest allocator failed to allocate {requested_bytes} bytes"
                )
            }
            Self::MemoryAccessOutOfBounds => {
                write!(f, "guest memory pointer is out of linear memory bounds")
            }
            Self::FuelExhausted => {
                write!(f, "sandboxed study exceeded its instruction fuel limit")
            }
            Self::MemoryLimitExceeded => {
                write!(f, "sandboxed study exceeded its linear memory limit")
            }
            Self::GuestTrap(msg) => write!(f, "sandboxed study trapped: {msg}"),
            Self::GuestError(code) => write!(f, "sandboxed study returned error code {code}"),
            Self::InvalidOutputValue { index } => {
                write!(
                    f,
                    "sandboxed study produced a non-finite infinite output at bar index {index}"
                )
            }
        }
    }
}

impl Error for WasmSandboxError {}

/// Execution telemetry returned alongside computed study points.
#[derive(Clone, Debug, PartialEq)]
pub struct WasmExecutionResult {
    /// Computed study output values aligned 1:1 with the input bars (`None` = hard gap).
    pub values: Vec<Option<f64>>,
    /// Exact instruction fuel consumed by this execution wave.
    pub fuel_consumed: u64,
}

/// Severity level for a runtime debug log emitted by a sandboxed WASM study.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WasmStudyLogLevel {
    Info,
    Warn,
    Error,
}

/// One bounded debug log entry emitted by a sandboxed WASM study during execution.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WasmStudyLogEntry {
    pub level: WasmStudyLogLevel,
    pub message: String,
}

/// Alignment policy for one secondary (MTF or multi-symbol) market dependency of a WASM study.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct WasmSecondaryRequestSpec {
    pub confirmation: StudyMtfConfirmation,
    pub gaps: StudyMtfGapPolicy,
}

/// Primary-aligned secondary OHLCV columns (`f64::NAN` where unaligned/gapped) passed into WASM.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WasmAlignedSecondary {
    pub open: Vec<f64>,
    pub high: Vec<f64>,
    pub low: Vec<f64>,
    pub close: Vec<f64>,
    pub volume: Vec<f64>,
}

/// Per-bar order-flow columns (`buy_vol`, `sell_vol`, `delta`, `trade_count`, `poc`) passed into WASM.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WasmOrderFlowColumns {
    pub buy_vol: Vec<f64>,
    pub sell_vol: Vec<f64>,
    pub delta: Vec<f64>,
    pub trade_count: Vec<f64>,
    pub poc: Vec<f64>,
}

/// Read-only instrument and execution metadata passed into a sandboxed WASM study.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WasmStudyMeta {
    pub timeframe_seconds: u32,
    pub price_scale: u8,
    pub quantity_scale: u8,
    pub is_realtime: bool,
    pub is_confirmed_bar: bool,
    pub is_new_bar: bool,
    pub symbol: String,
}

/// Bounded execution telemetry captured from the most recent sandboxed WASM study wave.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WasmStudyTelemetry {
    pub fuel_consumed: u64,
    pub fuel_limit: u64,
    pub elapsed_micros: u64,
    pub bar_count: usize,
    pub scene_object_count: usize,
    pub alert_count: usize,
    pub logs: Vec<WasmStudyLogEntry>,
    pub strategy_report: Option<crate::strategy::StrategyReport>,
}

/// Shared bounded sink for reading the latest execution telemetry of one live WASM study.
pub type WasmStudyTelemetrySink = Arc<Mutex<Option<WasmStudyTelemetry>>>;

/// Execution telemetry returned alongside multi-output computed study series.
#[derive(Clone, Debug, PartialEq)]
pub struct WasmMultiExecutionResult {
    /// One output vector per declared output series, each aligned 1:1 with the input bars.
    pub outputs: Vec<Vec<Option<f64>>>,
    /// Optional binary scene command buffer emitted by the guest study (boxes, lines, polylines,
    /// labels, tables, per-bar colors, background colors, alerts, logs, strategy commands).
    pub scene_bytes: Vec<u8>,
    /// Exact instruction fuel consumed by this execution wave.
    pub fuel_consumed: u64,
    /// Wall-clock duration of this execution wave in microseconds.
    pub elapsed_micros: u64,
    /// Decoded semantic alert events emitted during this wave (bounded).
    pub alerts: Vec<StudyAlertEvent>,
    /// Decoded runtime debug log entries emitted during this wave (bounded).
    pub logs: Vec<WasmStudyLogEntry>,
    /// Count of decoded scene objects in this wave.
    pub scene_object_count: usize,
    /// Strategy backtest report populated whenever the study emits strategy commands.
    pub strategy_report: Option<crate::strategy::StrategyReport>,
}

struct SandboxStoreState {
    limits: StoreLimits,
}

/// Persistent, bounded WebAssembly instance for O(dirty) incremental `aeris_study_on_bar` execution.
///
/// Reuses one `wasmi::Store` and one fixed 32-`f64` output scratch slot across live ticks while
/// strictly resetting per-step instruction fuel and enforcing linear memory ceilings.
pub struct WasmIncrementalInstance {
    store: Store<SandboxStoreState>,
    instance: wasmi::Instance,
    memory: wasmi::Memory,
    on_bar_fn: wasmi::TypedFunc<(u32, u32, f64, f64, f64, f64, f64, i64, u32, u32), i32>,
    out_ptr: u32,
    limits: StudySandboxLimits,
    last_bar_count: usize,
}

impl WasmIncrementalInstance {
    /// Steps one bar incrementally inside the persistent sandboxed store, returning `output_count`
    /// values and the exact instruction fuel consumed by this single bar step.
    ///
    /// # Errors
    /// Returns [`WasmSandboxError`] on fuel exhaustion, trap, guest error, or invalid output.
    #[allow(clippy::too_many_arguments)]
    pub fn step_bar(
        &mut self,
        bar_index: usize,
        is_new_bar: bool,
        is_confirmed: bool,
        open: f64,
        high: f64,
        low: f64,
        close: f64,
        volume: f64,
        timestamp_ns: i64,
        output_count: usize,
    ) -> Result<(Vec<Option<f64>>, u64), WasmSandboxError> {
        let count = output_count.clamp(1, 32);
        let initial_fuel = self.limits.max_instruction_fuel;
        self.store
            .set_fuel(initial_fuel)
            .map_err(|err| WasmSandboxError::InstantiationFailed(err.to_string()))?;
        let out_byte_len = count * size_of::<f64>();
        let nan_fill: Vec<u8> = f64::NAN
            .to_le_bytes()
            .into_iter()
            .cycle()
            .take(out_byte_len)
            .collect();
        self.memory
            .write(&mut self.store, self.out_ptr as usize, &nan_fill)
            .map_err(|_| WasmSandboxError::MemoryAccessOutOfBounds)?;
        let mut flags = 0_u32;
        if is_new_bar {
            flags |= 1;
        }
        if is_confirmed {
            flags |= 2;
        }
        let bar_u32 = u32::try_from(bar_index).unwrap_or(0);
        let count_u32 = u32::try_from(count).unwrap_or(1);
        let status = self
            .on_bar_fn
            .call(
                &mut self.store,
                (
                    bar_u32,
                    flags,
                    open,
                    high,
                    low,
                    close,
                    volume,
                    timestamp_ns,
                    self.out_ptr,
                    count_u32,
                ),
            )
            .map_err(|err| classify_wasmi_error(&err, false))?;
        if status != 0 {
            return Err(WasmSandboxError::GuestError(status));
        }
        let mut raw = vec![0_u8; out_byte_len];
        self.memory
            .read(&self.store, self.out_ptr as usize, &mut raw)
            .map_err(|_| WasmSandboxError::MemoryAccessOutOfBounds)?;
        let values = decode_output_f64_values(&raw, count)?;
        let remaining = self.store.get_fuel().unwrap_or(0);
        let fuel_consumed = initial_fuel.saturating_sub(remaining);
        Ok((values, fuel_consumed))
    }

    /// Reads the scene command buffer emitted during the most recent `step_bar` wave.
    ///
    /// # Errors
    /// Returns [`WasmSandboxError`] on trap or out-of-bounds memory read.
    pub fn read_scene_bytes(&mut self) -> Result<Vec<u8>, WasmSandboxError> {
        read_wasm_scene_buffer(self.instance, &mut self.store, self.memory)
    }
}

/// Pre-compiled, zero-authority WebAssembly study module validated against
/// `STUDY_SANDBOX_ABI_VERSION = 1`.
#[derive(Clone)]
pub struct CompiledWasmStudy {
    engine: Engine,
    module: Arc<Module>,
}

impl fmt::Debug for CompiledWasmStudy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CompiledWasmStudy").finish_non_exhaustive()
    }
}

impl CompiledWasmStudy {
    /// Compiles and validates raw `.wasm` bytes against the zero-authority sandbox policy
    /// and probes `aeris_study_abi_version()` under bounded fuel and memory limits.
    ///
    /// # Errors
    /// Returns [`WasmSandboxError`] if the artifact is empty/oversized, fails validation,
    /// declares any host import, misses required exports, or mismatches `STUDY_SANDBOX_ABI_VERSION`.
    pub fn from_bytes(
        wasm_bytes: &[u8],
        limits: StudySandboxLimits,
    ) -> Result<Self, WasmSandboxError> {
        if wasm_bytes.is_empty() {
            return Err(WasmSandboxError::EmptyArtifact);
        }
        if wasm_bytes.len() > MAXIMUM_SANDBOX_ARTIFACT_BYTES {
            return Err(WasmSandboxError::ArtifactTooLarge {
                maximum: MAXIMUM_SANDBOX_ARTIFACT_BYTES,
                actual: wasm_bytes.len(),
            });
        }

        let mut config = Config::default();
        config.consume_fuel(true);
        let engine = Engine::new(&config);
        let module = Module::new(&engine, wasm_bytes)
            .map_err(|err| WasmSandboxError::CompileFailed(err.to_string()))?;

        // Enforce zero ambient authority: no host imports are permitted.
        if let Some(import) = module.imports().next() {
            return Err(WasmSandboxError::ImportDenied {
                module: import.module().to_string(),
                name: import.name().to_string(),
            });
        }

        let compiled = Self {
            engine,
            module: Arc::new(module),
        };
        compiled.verify_abi_and_exports(limits)?;
        Ok(compiled)
    }

    fn create_store(
        &self,
        limits: StudySandboxLimits,
    ) -> Result<Store<SandboxStoreState>, WasmSandboxError> {
        let store_limits = StoreLimitsBuilder::new()
            .memory_size(limits.max_memory_bytes)
            .tables(16)
            .instances(1)
            .memories(1)
            .build();
        let mut store = Store::new(
            &self.engine,
            SandboxStoreState {
                limits: store_limits,
            },
        );
        store.limiter(|state| &mut state.limits);
        store
            .set_fuel(limits.max_instruction_fuel)
            .map_err(|err| WasmSandboxError::InstantiationFailed(err.to_string()))?;
        Ok(store)
    }

    fn verify_abi_and_exports(&self, limits: StudySandboxLimits) -> Result<(), WasmSandboxError> {
        let mut store = self.create_store(limits)?;
        let linker = Linker::<SandboxStoreState>::new(&self.engine);
        let instance = linker
            .instantiate_and_start(&mut store, &self.module)
            .map_err(|err| classify_wasmi_error(&err, true))?;

        let _memory = instance
            .get_memory(&store, WASM_EXPORT_MEMORY)
            .ok_or(WasmSandboxError::MissingExport(WASM_EXPORT_MEMORY))?;
        let abi_fn = instance
            .get_typed_func::<(), u32>(&store, WASM_EXPORT_ABI_VERSION)
            .map_err(|_| WasmSandboxError::MissingExport(WASM_EXPORT_ABI_VERSION))?;
        let _alloc_fn = instance
            .get_typed_func::<u32, u32>(&store, WASM_EXPORT_ALLOC)
            .map_err(|_| WasmSandboxError::MissingExport(WASM_EXPORT_ALLOC))?;
        let _calc_fn = instance
            .get_typed_func::<(u32, u32, u32), i32>(&store, WASM_EXPORT_CALCULATE)
            .map_err(|_| WasmSandboxError::MissingExport(WASM_EXPORT_CALCULATE))?;

        let actual_abi = abi_fn
            .call(&mut store, ())
            .map_err(|err| classify_wasmi_error(&err, false))?;
        if actual_abi != STUDY_SANDBOX_ABI_VERSION {
            return Err(WasmSandboxError::AbiVersionMismatch {
                expected: STUDY_SANDBOX_ABI_VERSION,
                actual: actual_abi,
            });
        }
        Ok(())
    }

    /// Executes this sandboxed study over one columnized OHLCV bar batch in a fresh isolated
    /// `wasmi::Store` bounded by `limits`, returning `output_count` output series.
    ///
    /// # Errors
    /// Returns [`WasmSandboxError`] on inconsistent input, fuel exhaustion, memory ceiling
    /// violation, guest trap, guest error code, or non-finite infinite output.
    pub fn execute_columnar_outputs(
        &self,
        bars: &StudySandboxColumnarBars,
        output_count: usize,
        limits: StudySandboxLimits,
    ) -> Result<WasmMultiExecutionResult, WasmSandboxError> {
        self.execute_columnar_with_params(bars, output_count, &[], limits)
    }

    /// Executes this sandboxed study with optional numeric input parameters passed via
    /// `aeris_study_set_param` before calculation.
    ///
    /// # Errors
    /// Returns [`WasmSandboxError`] on inconsistent input, fuel exhaustion, memory ceiling
    /// violation, guest trap, guest error code, or non-finite infinite output.
    pub fn execute_columnar_with_params(
        &self,
        bars: &StudySandboxColumnarBars,
        output_count: usize,
        params: &[f64],
        limits: StudySandboxLimits,
    ) -> Result<WasmMultiExecutionResult, WasmSandboxError> {
        self.execute_columnar_full(bars, output_count, params, None, limits)
    }

    /// Returns `true` if this compiled module exports the optional O(dirty) incremental
    /// `aeris_study_on_bar` entrypoint.
    #[must_use]
    pub fn supports_incremental(&self) -> bool {
        self.module
            .exports()
            .any(|export| export.name() == WASM_EXPORT_ON_BAR)
    }

    /// Creates a persistent bounded [`WasmIncrementalInstance`] initialized with parameters and
    /// instrument metadata, ready for O(dirty) live-tick `step_bar` calls.
    ///
    /// # Errors
    /// Returns [`WasmSandboxError`] if the module does not export `aeris_study_on_bar` or fails
    /// instantiation/initialization.
    pub fn instantiate_incremental(
        &self,
        params: &[f64],
        str_params: &[(usize, String)],
        meta: &WasmStudyMeta,
        limits: StudySandboxLimits,
    ) -> Result<WasmIncrementalInstance, WasmSandboxError> {
        let mut store = self.create_store(limits)?;
        let linker = Linker::<SandboxStoreState>::new(&self.engine);
        let instance = linker
            .instantiate_and_start(&mut store, &self.module)
            .map_err(|err| classify_wasmi_error(&err, true))?;
        let memory = instance
            .get_memory(&store, WASM_EXPORT_MEMORY)
            .ok_or(WasmSandboxError::MissingExport(WASM_EXPORT_MEMORY))?;
        let alloc_fn = instance
            .get_typed_func::<u32, u32>(&store, WASM_EXPORT_ALLOC)
            .map_err(|_| WasmSandboxError::MissingExport(WASM_EXPORT_ALLOC))?;
        let on_bar_fn = instance
            .get_typed_func::<(u32, u32, f64, f64, f64, f64, f64, i64, u32, u32), i32>(
                &store,
                WASM_EXPORT_ON_BAR,
            )
            .map_err(|_| WasmSandboxError::MissingExport(WASM_EXPORT_ON_BAR))?;
        apply_wasm_params(instance, &mut store, params)?;
        apply_wasm_str_params(instance, &mut store, memory, alloc_fn, str_params)?;
        apply_wasm_meta(instance, &mut store, memory, alloc_fn, meta)?;
        let scratch_bytes = 32 * size_of::<f64>();
        let out_ptr = alloc_guest(&mut store, alloc_fn, scratch_bytes as u32, scratch_bytes)?;
        Ok(WasmIncrementalInstance {
            store,
            instance,
            memory,
            on_bar_fn,
            out_ptr,
            limits,
            last_bar_count: 0,
        })
    }

    /// Executes a single- or multi-output sandboxed study with optional parameter overrides and
    /// visible chart viewport range `(left_bar, right_bar)`.
    ///
    /// # Errors
    /// Returns [`WasmSandboxError`] on inconsistent input, fuel exhaustion, memory ceiling
    /// violation, guest trap, guest error code, or non-finite infinite output.
    pub fn execute_columnar_full(
        &self,
        bars: &StudySandboxColumnarBars,
        output_count: usize,
        params: &[f64],
        viewport: Option<(usize, usize)>,
        limits: StudySandboxLimits,
    ) -> Result<WasmMultiExecutionResult, WasmSandboxError> {
        let meta = WasmStudyMeta {
            timeframe_seconds: u32::try_from(
                estimate_bar_step_ns(&bars.timestamps_unix_nanos) / 1_000_000_000,
            )
            .unwrap_or(60),
            price_scale: bars.price_scale,
            quantity_scale: bars.quantity_scale,
            is_realtime: false,
            is_confirmed_bar: true,
            is_new_bar: false,
            symbol: String::new(),
        };
        self.execute_columnar_with_meta(bars, output_count, params, viewport, &meta, limits)
    }

    /// Executes a single- or multi-output sandboxed study with parameters, visible chart viewport
    /// range, and full instrument/execution metadata (`timestamps_unix_nanos`, `syminfo`).
    ///
    /// # Errors
    /// Returns [`WasmSandboxError`] on inconsistent input, fuel exhaustion, memory ceiling
    /// violation, guest trap, guest error code, or non-finite infinite output.
    pub fn execute_columnar_with_meta(
        &self,
        bars: &StudySandboxColumnarBars,
        output_count: usize,
        params: &[f64],
        viewport: Option<(usize, usize)>,
        meta: &WasmStudyMeta,
        limits: StudySandboxLimits,
    ) -> Result<WasmMultiExecutionResult, WasmSandboxError> {
        self.execute_columnar_with_typed_params(
            bars,
            output_count,
            params,
            &[],
            viewport,
            meta,
            limits,
        )
    }

    /// Executes a single- or multi-output sandboxed study with numeric + UTF-8 string parameters,
    /// visible chart viewport range, and full instrument/execution metadata.
    ///
    /// # Errors
    /// Returns [`WasmSandboxError`] on inconsistent input, fuel exhaustion, memory ceiling
    /// violation, guest trap, guest error code, or non-finite infinite output.
    #[allow(clippy::too_many_arguments)]
    pub fn execute_columnar_with_typed_params(
        &self,
        bars: &StudySandboxColumnarBars,
        output_count: usize,
        params: &[f64],
        str_params: &[(usize, String)],
        viewport: Option<(usize, usize)>,
        meta: &WasmStudyMeta,
        limits: StudySandboxLimits,
    ) -> Result<WasmMultiExecutionResult, WasmSandboxError> {
        self.execute_columnar_extended(
            bars,
            output_count,
            params,
            str_params,
            &[],
            None,
            viewport,
            meta,
            limits,
        )
    }

    /// Executes a sandboxed study with full typed parameters, aligned secondary (MTF / multi-symbol)
    /// series, optional per-bar order-flow columns, viewport range, and instrument metadata.
    ///
    /// # Errors
    /// Returns [`WasmSandboxError`] on inconsistent input, fuel exhaustion, memory ceiling
    /// violation, guest trap, guest error code, or non-finite infinite output.
    #[allow(clippy::too_many_arguments)]
    pub fn execute_columnar_extended(
        &self,
        bars: &StudySandboxColumnarBars,
        output_count: usize,
        params: &[f64],
        str_params: &[(usize, String)],
        secondaries: &[WasmAlignedSecondary],
        order_flow: Option<&WasmOrderFlowColumns>,
        viewport: Option<(usize, usize)>,
        meta: &WasmStudyMeta,
        limits: StudySandboxLimits,
    ) -> Result<WasmMultiExecutionResult, WasmSandboxError> {
        let started_at = Instant::now();
        let count = output_count.clamp(1, 32);
        if !bars.is_consistent() {
            return Err(WasmSandboxError::InconsistentInputBars);
        }
        let bar_count = bars.len();
        if bar_count == 0 {
            return Ok(WasmMultiExecutionResult {
                outputs: vec![Vec::new(); count],
                scene_bytes: Vec::new(),
                fuel_consumed: 0,
                elapsed_micros: 0,
                alerts: Vec::new(),
                logs: Vec::new(),
                scene_object_count: 0,
                strategy_report: None,
            });
        }
        if bar_count > limits.max_output_points {
            return Err(WasmSandboxError::InputTooLarge {
                maximum: limits.max_output_points,
                actual: bar_count,
            });
        }

        let mut store = self.create_store(limits)?;
        let initial_fuel = limits.max_instruction_fuel;
        let linker = Linker::<SandboxStoreState>::new(&self.engine);
        let instance = linker
            .instantiate_and_start(&mut store, &self.module)
            .map_err(|err| classify_wasmi_error(&err, true))?;

        let memory = instance
            .get_memory(&store, WASM_EXPORT_MEMORY)
            .ok_or(WasmSandboxError::MissingExport(WASM_EXPORT_MEMORY))?;
        let alloc_fn = instance
            .get_typed_func::<u32, u32>(&store, WASM_EXPORT_ALLOC)
            .map_err(|_| WasmSandboxError::MissingExport(WASM_EXPORT_ALLOC))?;
        let multi_fn = instance
            .get_typed_func::<(u32, u32, u32, u32), i32>(&store, WASM_EXPORT_CALCULATE_MULTI)
            .ok();
        let single_fn = instance
            .get_typed_func::<(u32, u32, u32), i32>(&store, WASM_EXPORT_CALCULATE)
            .map_err(|_| WasmSandboxError::MissingExport(WASM_EXPORT_CALCULATE))?;
        apply_wasm_params(instance, &mut store, params)?;
        apply_wasm_str_params(instance, &mut store, memory, alloc_fn, str_params)?;
        apply_wasm_viewport(instance, &mut store, viewport, bar_count)?;
        apply_wasm_timestamps(
            instance,
            &mut store,
            memory,
            alloc_fn,
            &bars.timestamps_unix_nanos,
        )?;
        apply_wasm_meta(instance, &mut store, memory, alloc_fn, meta)?;
        apply_wasm_secondaries(instance, &mut store, memory, alloc_fn, secondaries, bar_count)?;
        if let Some(of) = order_flow {
            apply_wasm_order_flow(instance, &mut store, memory, alloc_fn, of, bar_count)?;
        }

        let input_bytes = pack_columnar_ohlcv_f64(bars);
        let total_out_values =
            bar_count
                .checked_mul(count)
                .ok_or(WasmSandboxError::AllocationFailed {
                    requested_bytes: usize::MAX,
                })?;
        let out_byte_len = total_out_values.checked_mul(size_of::<f64>()).ok_or(
            WasmSandboxError::AllocationFailed {
                requested_bytes: usize::MAX,
            },
        )?;
        let (input_len_u32, out_len_u32, bar_count_u32, out_count_u32) =
            checked_wasm_sizes(input_bytes.len(), out_byte_len, bar_count, count, limits)?;

        let bars_ptr = alloc_guest(&mut store, alloc_fn, input_len_u32, input_bytes.len())?;
        let out_ptr = alloc_guest(&mut store, alloc_fn, out_len_u32, out_byte_len)?;

        memory
            .write(&mut store, bars_ptr as usize, &input_bytes)
            .map_err(|_| WasmSandboxError::MemoryAccessOutOfBounds)?;
        let nan_fill: Vec<u8> = f64::NAN
            .to_le_bytes()
            .into_iter()
            .cycle()
            .take(out_byte_len)
            .collect();
        memory
            .write(&mut store, out_ptr as usize, &nan_fill)
            .map_err(|_| WasmSandboxError::MemoryAccessOutOfBounds)?;

        let status = if let Some(multi) = multi_fn {
            multi
                .call(
                    &mut store,
                    (bars_ptr, bar_count_u32, out_ptr, out_count_u32),
                )
                .map_err(|err| classify_wasmi_error(&err, false))?
        } else {
            single_fn
                .call(&mut store, (bars_ptr, bar_count_u32, out_ptr))
                .map_err(|err| classify_wasmi_error(&err, false))?
        };
        if status != 0 {
            return Err(WasmSandboxError::GuestError(status));
        }

        let mut raw_output = vec![0_u8; out_byte_len];
        memory
            .read(&store, out_ptr as usize, &mut raw_output)
            .map_err(|_| WasmSandboxError::MemoryAccessOutOfBounds)?;
        let flat = decode_output_f64_values(&raw_output, total_out_values)?;
        let outputs: Vec<Vec<Option<f64>>> = flat
            .chunks_exact(bar_count)
            .map(<[Option<f64>]>::to_vec)
            .collect();
        let scene_bytes = read_wasm_scene_buffer(instance, &mut store, memory)?;
        let (alerts, logs, scene_object_count, strategy_report) = if scene_bytes.is_empty() {
            (Vec::new(), Vec::new(), 0, None)
        } else {
            let decoded = decode_wasm_scene_commands(&scene_bytes, bars);
            let report = (!decoded.strategy_commands.is_empty()).then(|| {
                let tick_size = 10_f64.powi(-i32::from(bars.price_scale));
                crate::strategy::evaluate_strategy(
                    bars,
                    &decoded.strategy_commands,
                    crate::strategy::StrategyConfig {
                        tick_size,
                        ..crate::strategy::StrategyConfig::default()
                    },
                )
            });
            (decoded.alerts, decoded.logs, decoded.objects.len(), report)
        };

        let remaining_fuel = store.get_fuel().unwrap_or(0);
        let fuel_consumed = initial_fuel.saturating_sub(remaining_fuel);
        let elapsed_micros = u64::try_from(started_at.elapsed().as_micros()).unwrap_or(u64::MAX);
        Ok(WasmMultiExecutionResult {
            outputs,
            scene_bytes,
            fuel_consumed,
            elapsed_micros,
            alerts,
            logs,
            scene_object_count,
            strategy_report,
        })
    }

    /// Executes a single-output sandboxed study over one columnized OHLCV bar batch.
    ///
    /// # Errors
    /// Returns [`WasmSandboxError`] on failure.
    pub fn execute_columnar(
        &self,
        bars: &StudySandboxColumnarBars,
        limits: StudySandboxLimits,
    ) -> Result<WasmExecutionResult, WasmSandboxError> {
        let multi = self.execute_columnar_outputs(bars, 1, limits)?;
        Ok(WasmExecutionResult {
            values: multi.outputs.into_iter().next().unwrap_or_default(),
            fuel_consumed: multi.fuel_consumed,
        })
    }

    /// Wraps this compiled sandboxed WebAssembly module into a [`NativeStudyProgram`] that runs
    /// directly inside `StudyRuntime` with transactional rollback and generation fencing.
    #[must_use]
    pub fn into_native_program(self, limits: StudySandboxLimits) -> NativeStudyProgram {
        self.into_native_program_with_spec(1, Vec::new(), Vec::new(), Vec::new(), limits)
    }

    /// Wraps a multi-output sandboxed WebAssembly module (with optional typed setting names,
    /// fills, and horizontal reference levels) into a [`NativeStudyProgram`] that runs inside
    /// `StudyRuntime`.
    ///
    /// Writes only rows inside `context.dirty_range()` on incremental live ticks so live market
    /// updates remain O(dirty) on the persistent output trees instead of rewriting all history.
    #[must_use]
    pub fn into_native_program_with_spec(
        self,
        output_count: usize,
        param_identifiers: Vec<String>,
        fills: Vec<StudyFillSpec>,
        levels: Vec<StudyLevelSpec>,
        limits: StudySandboxLimits,
    ) -> NativeStudyProgram {
        self.into_native_program_with_spec_and_telemetry(
            output_count,
            param_identifiers,
            fills,
            levels,
            limits,
            None,
        )
    }

    /// Wraps a multi-output sandboxed WebAssembly module with optional telemetry sink so the
    /// Study Editor Console can observe live execution fuel, duration, scene counts, and `log_*`
    /// output from the running chart instance.
    #[must_use]
    pub fn into_native_program_with_spec_and_telemetry(
        self,
        output_count: usize,
        param_identifiers: Vec<String>,
        fills: Vec<StudyFillSpec>,
        levels: Vec<StudyLevelSpec>,
        limits: StudySandboxLimits,
        telemetry_sink: Option<WasmStudyTelemetrySink>,
    ) -> NativeStudyProgram {
        self.into_native_program_full(
            output_count,
            param_identifiers,
            Vec::new(),
            false,
            fills,
            levels,
            limits,
            telemetry_sink,
        )
    }

    /// Full WASM study program bridge supporting secondary MTF / multi-symbol market dependencies,
    /// per-bar order-flow columns, typed parameters, fills, levels, and live telemetry.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn into_native_program_full(
        self,
        output_count: usize,
        param_identifiers: Vec<String>,
        secondary_specs: Vec<WasmSecondaryRequestSpec>,
        include_order_flow: bool,
        fills: Vec<StudyFillSpec>,
        levels: Vec<StudyLevelSpec>,
        limits: StudySandboxLimits,
        telemetry_sink: Option<WasmStudyTelemetrySink>,
    ) -> NativeStudyProgram {
        let compiled = self;
        let count = output_count.clamp(1, 32);
        let persistent_instance: Arc<Mutex<Option<WasmIncrementalInstance>>> =
            Arc::new(Mutex::new(None));
        NativeStudyProgram::dynamic(Arc::new(move |context: &mut StudyExecutionContext<'_>| {
            let Some(StudyInputSeries::Market(market)) = context.input(0) else {
                return Err("sandboxed study requires primary market bar input".to_string());
            };
            let bars = market.bars();
            let bar_count = bars.len();
            if bar_count == 0 {
                return Ok(());
            }
            let exec_meta = context.meta();
            let series_key = market.series();
            let timeframe_seconds = bar_period_to_seconds(series_key.period);
            let symbol = series_key.instrument_id.clone();
            let dirty = context.dirty_range();
            let dirty_start = dirty.start.min(bar_count);
            let dirty_end = dirty.end_exclusive.unwrap_or(bar_count).min(bar_count);
            let mut params = Vec::with_capacity(param_identifiers.len());
            let mut str_params = Vec::new();
            for (idx, id) in param_identifiers.iter().enumerate() {
                match context.settings().get(id) {
                    Some(StudySettingValue::Integer(v)) => {
                        params.push(fixed_point_to_f64(*v, 0).unwrap_or(f64::NAN));
                    }
                    Some(StudySettingValue::Decimal(d) | StudySettingValue::Price(d)) => {
                        params.push(fixed_point_to_f64(d.mantissa, d.scale).unwrap_or(f64::NAN));
                    }
                    Some(StudySettingValue::Boolean(b)) => {
                        params.push(if *b { 1.0 } else { 0.0 });
                    }
                    Some(StudySettingValue::Color(c)) => {
                        params.push(f64::from(c.to_rgba_u32()));
                    }
                    Some(StudySettingValue::Source(src)) => {
                        let code = match src {
                            StudySource::Open => 0.0,
                            StudySource::High => 1.0,
                            StudySource::Low => 2.0,
                            StudySource::Close => 3.0,
                            StudySource::Hl2 => 4.0,
                            StudySource::Hlc3 => 5.0,
                            StudySource::Ohlc4 => 6.0,
                            StudySource::Volume => 7.0,
                            StudySource::StudyOutput { .. } => 3.0,
                        };
                        params.push(code);
                    }
                    Some(StudySettingValue::Choice(choice)) => {
                        params.push(f64::NAN);
                        str_params.push((idx, choice.clone()));
                    }
                    Some(StudySettingValue::Text(text)) => {
                        params.push(f64::NAN);
                        str_params.push((idx, text.clone()));
                    }
                    Some(StudySettingValue::Timeframe(tf)) => {
                        let secs = match tf {
                            crate::StudyTimeframe::CurrentChart => timeframe_seconds,
                            crate::StudyTimeframe::Period(p) => bar_period_to_seconds(*p),
                        };
                        params.push(f64::from(secs));
                        str_params.push((idx, tf.to_label()));
                    }
                    Some(StudySettingValue::Session(sess)) => {
                        params.push(f64::from(sess.start_minute));
                        str_params.push((idx, sess.to_label()));
                    }
                    Some(StudySettingValue::Symbol(sym)) => {
                        params.push(f64::NAN);
                        str_params.push((idx, sym.to_label()));
                    }
                    Some(StudySettingValue::Timestamp(ts)) => {
                        params.push(*ts as f64);
                    }
                    None => params.push(f64::NAN),
                }
            }

            // Clamp to the newest `limits.max_output_points` bars if the input window is larger.
            let start = bar_count.saturating_sub(limits.max_output_points);
            let slice_len = bar_count.saturating_sub(start);
            let mut columnar = StudySandboxColumnarBars {
                price_scale: bars.price_scale(),
                quantity_scale: bars.quantity_scale(),
                timestamps_unix_nanos: Vec::with_capacity(slice_len),
                open: Vec::with_capacity(slice_len),
                high: Vec::with_capacity(slice_len),
                low: Vec::with_capacity(slice_len),
                close: Vec::with_capacity(slice_len),
                volume: Vec::with_capacity(slice_len),
            };
            for idx in start..bar_count {
                if let Some(bar) = bars.get(idx) {
                    columnar
                        .timestamps_unix_nanos
                        .push(bar.exchange_timestamp_unix_nanos);
                    columnar.open.push(bar.open);
                    columnar.high.push(bar.high);
                    columnar.low.push(bar.low);
                    columnar.close.push(bar.close);
                    columnar.volume.push(bar.volume);
                }
            }

            let viewport = resolve_visible_bar_range(
                &columnar.timestamps_unix_nanos,
                context.chart_context().visible_start_unix_nanos,
                context.chart_context().visible_end_unix_nanos,
            );
            let wasm_meta = WasmStudyMeta {
                timeframe_seconds: if timeframe_seconds > 0 {
                    timeframe_seconds
                } else {
                    u32::try_from(
                        estimate_bar_step_ns(&columnar.timestamps_unix_nanos) / 1_000_000_000,
                    )
                    .unwrap_or(60)
                },
                price_scale: columnar.price_scale,
                quantity_scale: columnar.quantity_scale,
                is_realtime: exec_meta.is_realtime(),
                is_confirmed_bar: exec_meta.is_confirmed_bar,
                is_new_bar: exec_meta.is_new_bar,
                symbol,
            };
            if compiled.supports_incremental() && secondary_specs.is_empty() && !include_order_flow
            {
                let started_at = Instant::now();
                let mut guard = persistent_instance
                    .lock()
                    .map_err(|_| "incremental wasm store lock poisoned".to_string())?;
                let needs_reset = dirty_start == 0
                    || guard
                        .as_ref()
                        .is_none_or(|inst| bar_count < inst.last_bar_count);
                if needs_reset {
                    *guard = Some(
                        compiled
                            .instantiate_incremental(&params, &str_params, &wasm_meta, limits)
                            .map_err(|err| err.to_string())?,
                    );
                }
                let Some(inst) = guard.as_mut() else {
                    return Err("incremental wasm instance unavailable".to_string());
                };
                let write_from = if needs_reset { start } else { dirty_start.max(start) };
                let write_to = dirty_end.min(bar_count);
                let mut total_fuel = 0_u64;
                for row in write_from..write_to {
                    let local = row - start;
                    let o = fixed_point_to_f64(columnar.open[local], columnar.price_scale)
                        .unwrap_or(f64::NAN);
                    let h = fixed_point_to_f64(columnar.high[local], columnar.price_scale)
                        .unwrap_or(f64::NAN);
                    let l = fixed_point_to_f64(columnar.low[local], columnar.price_scale)
                        .unwrap_or(f64::NAN);
                    let c = fixed_point_to_f64(columnar.close[local], columnar.price_scale)
                        .unwrap_or(f64::NAN);
                    let v = fixed_point_to_f64(columnar.volume[local], columnar.quantity_scale)
                        .unwrap_or(f64::NAN);
                    let ts = columnar.timestamps_unix_nanos[local];
                    let is_last = row + 1 == bar_count;
                    let is_new = row >= inst.last_bar_count;
                    let is_conf = !is_last || exec_meta.is_confirmed_bar;
                    match inst.step_bar(local, is_new, is_conf, o, h, l, c, v, ts, count) {
                        Ok((vals, fuel)) => {
                            total_fuel = total_fuel.saturating_add(fuel);
                            for (out_idx, val) in vals.into_iter().enumerate() {
                                if let Some(output) = context.output(out_idx) {
                                    output.set(row, val)?;
                                }
                            }
                        }
                        Err(err) => {
                            *guard = None;
                            return Err(err.to_string());
                        }
                    }
                }
                inst.last_bar_count = bar_count;
                let scene_bytes = inst.read_scene_bytes().unwrap_or_default();
                let mut scene_objs = 0usize;
                let mut alert_count = 0usize;
                let mut logs = Vec::new();
                if !scene_bytes.is_empty() {
                    let decoded = decode_wasm_scene_commands(&scene_bytes, &columnar);
                    scene_objs = decoded.objects.len();
                    alert_count = decoded.alerts.len();
                    logs = decoded.logs;
                    apply_decoded_wasm_scene(
                        context,
                        &scene_bytes,
                        &columnar,
                        start,
                        write_from,
                        write_to,
                    )?;
                }
                if let Some(sink) = &telemetry_sink
                    && let Ok(mut t) = sink.lock()
                {
                    *t = Some(WasmStudyTelemetry {
                        fuel_consumed: total_fuel,
                        fuel_limit: limits.max_instruction_fuel,
                        elapsed_micros: u64::try_from(started_at.elapsed().as_micros())
                            .unwrap_or(u64::MAX),
                        bar_count: write_to.saturating_sub(write_from),
                        scene_object_count: scene_objs,
                        alert_count,
                        logs,
                        strategy_report: None,
                    });
                }
                if dirty_start == 0 && dirty_end == bar_count {
                    for &fill in &fills {
                        context.add_fill(fill)?;
                    }
                    for level in &levels {
                        context.add_level(level.clone())?;
                    }
                }
                return Ok(());
            }

            let secondaries: Vec<WasmAlignedSecondary> = secondary_specs
                .iter()
                .take(MAX_WASM_SECONDARY_SERIES)
                .enumerate()
                .map(|(slot_idx, req)| {
                    if let Some(StudyInputSeries::Market(sec_market)) =
                        context.input(slot_idx + 1)
                    {
                        align_secondary_market_to_primary(
                            &columnar.timestamps_unix_nanos,
                            sec_market,
                            StudyRequestContext {
                                confirmation: req.confirmation,
                                gaps: req.gaps,
                            },
                        )
                    } else {
                        WasmAlignedSecondary {
                            open: vec![f64::NAN; slice_len],
                            high: vec![f64::NAN; slice_len],
                            low: vec![f64::NAN; slice_len],
                            close: vec![f64::NAN; slice_len],
                            volume: vec![f64::NAN; slice_len],
                        }
                    }
                })
                .collect();
            let order_flow = include_order_flow.then(|| {
                aggregate_order_flow_columns(
                    &columnar,
                    context.live_market(0).and_then(|m| m.trades()),
                )
            });
            let result = compiled
                .execute_columnar_extended(
                    &columnar,
                    count,
                    &params,
                    &str_params,
                    &secondaries,
                    order_flow.as_ref(),
                    viewport,
                    &wasm_meta,
                    limits,
                )
                .map_err(|err| err.to_string())?;

            let write_from = dirty_start.max(start);
            let write_to = dirty_end.min(bar_count);
            for (out_idx, series) in result.outputs.into_iter().enumerate() {
                let Some(output) = context.output(out_idx) else {
                    break;
                };
                for row in write_from..write_to {
                    if let Some(&value) = series.get(row - start) {
                        output.set(row, value)?;
                    }
                }
            }
            if !result.scene_bytes.is_empty() {
                apply_decoded_wasm_scene(
                    context,
                    &result.scene_bytes,
                    &columnar,
                    start,
                    write_from,
                    write_to,
                )?;
            }
            if let Some(sink) = &telemetry_sink
                && let Ok(mut guard) = sink.lock()
            {
                *guard = Some(WasmStudyTelemetry {
                    fuel_consumed: result.fuel_consumed,
                    fuel_limit: limits.max_instruction_fuel,
                    elapsed_micros: result.elapsed_micros,
                    bar_count: slice_len,
                    scene_object_count: result.scene_object_count,
                    alert_count: result.alerts.len(),
                    logs: result.logs,
                    strategy_report: result.strategy_report,
                });
            }
            if dirty_start == 0 && dirty_end == bar_count {
                for &fill in &fills {
                    context.add_fill(fill)?;
                }
                for level in &levels {
                    context.add_level(level.clone())?;
                }
            }
            Ok(())
        }))
    }
}

fn apply_wasm_params(
    instance: wasmi::Instance,
    store: &mut Store<SandboxStoreState>,
    params: &[f64],
) -> Result<(), WasmSandboxError> {
    if params.is_empty() {
        return Ok(());
    }
    let Ok(set_param_fn) =
        instance.get_typed_func::<(u32, f64), ()>(&*store, WASM_EXPORT_SET_PARAM)
    else {
        return Ok(());
    };
    for (idx, &val) in params.iter().take(MAX_WASM_PARAMS).enumerate() {
        if let Ok(idx_u32) = u32::try_from(idx) {
            set_param_fn
                .call(&mut *store, (idx_u32, val))
                .map_err(|err| classify_wasmi_error(&err, false))?;
        }
    }
    Ok(())
}

fn apply_wasm_str_params(
    instance: wasmi::Instance,
    store: &mut Store<SandboxStoreState>,
    memory: wasmi::Memory,
    alloc_fn: wasmi::TypedFunc<u32, u32>,
    str_params: &[(usize, String)],
) -> Result<(), WasmSandboxError> {
    if str_params.is_empty() {
        return Ok(());
    }
    let Ok(set_str_fn) =
        instance.get_typed_func::<(u32, u32, u32), ()>(&*store, WASM_EXPORT_SET_STR_PARAM)
    else {
        return Ok(());
    };
    for (idx, value) in str_params {
        if *idx >= MAX_WASM_PARAMS {
            continue;
        }
        let bytes = value.as_bytes();
        let len = bytes.len().min(MAX_WASM_STR_PARAM_BYTES);
        let (ptr, len_u32) = if len > 0 {
            let l = u32::try_from(len).unwrap_or(0);
            let p = alloc_guest(store, alloc_fn, l, len)?;
            memory
                .write(&mut *store, p as usize, &bytes[..len])
                .map_err(|_| WasmSandboxError::MemoryAccessOutOfBounds)?;
            (p, l)
        } else {
            (0, 0)
        };
        if let Ok(idx_u32) = u32::try_from(*idx) {
            set_str_fn
                .call(&mut *store, (idx_u32, ptr, len_u32))
                .map_err(|err| classify_wasmi_error(&err, false))?;
        }
    }
    Ok(())
}

fn bar_period_to_seconds(period: BarPeriod) -> u32 {
    match period {
        BarPeriod::Time { seconds } => seconds,
        BarPeriod::Session { days } => u32::from(days).saturating_mul(86_400),
        _ => 0,
    }
}

fn apply_wasm_timestamps(
    instance: wasmi::Instance,
    store: &mut Store<SandboxStoreState>,
    memory: wasmi::Memory,
    alloc_fn: wasmi::TypedFunc<u32, u32>,
    timestamps: &[i64],
) -> Result<(), WasmSandboxError> {
    if timestamps.is_empty() {
        return Ok(());
    }
    let Ok(set_ts_fn) =
        instance.get_typed_func::<(u32, u32), ()>(&*store, WASM_EXPORT_SET_TIMESTAMPS)
    else {
        return Ok(());
    };
    let byte_len =
        timestamps
            .len()
            .checked_mul(size_of::<i64>())
            .ok_or(WasmSandboxError::AllocationFailed {
                requested_bytes: usize::MAX,
            })?;
    let byte_len_u32 = u32::try_from(byte_len).map_err(|_| WasmSandboxError::AllocationFailed {
        requested_bytes: byte_len,
    })?;
    let count_u32 = u32::try_from(timestamps.len()).unwrap_or(0);
    let ptr = alloc_guest(store, alloc_fn, byte_len_u32, byte_len)?;
    let mut raw = Vec::with_capacity(byte_len);
    for &ts in timestamps {
        raw.extend_from_slice(&ts.to_le_bytes());
    }
    memory
        .write(&mut *store, ptr as usize, &raw)
        .map_err(|_| WasmSandboxError::MemoryAccessOutOfBounds)?;
    set_ts_fn
        .call(&mut *store, (ptr, count_u32))
        .map_err(|err| classify_wasmi_error(&err, false))
}

fn apply_wasm_meta(
    instance: wasmi::Instance,
    store: &mut Store<SandboxStoreState>,
    memory: wasmi::Memory,
    alloc_fn: wasmi::TypedFunc<u32, u32>,
    meta: &WasmStudyMeta,
) -> Result<(), WasmSandboxError> {
    let Ok(set_meta_fn) = instance
        .get_typed_func::<(u32, u32, u32, u32, u32, u32), ()>(&*store, WASM_EXPORT_SET_META)
    else {
        return Ok(());
    };
    let mut flags = 0_u32;
    if meta.is_realtime {
        flags |= META_FLAG_REALTIME;
    }
    if meta.is_confirmed_bar {
        flags |= META_FLAG_CONFIRMED_BAR;
    }
    if meta.is_new_bar {
        flags |= META_FLAG_NEW_BAR;
    }
    let sym_bytes = meta.symbol.as_bytes();
    let sym_len = sym_bytes.len().min(MAX_WASM_SYMBOL_BYTES);
    let (sym_ptr, sym_len_u32) = if sym_len > 0 {
        let len_u32 = u32::try_from(sym_len).unwrap_or(0);
        let ptr = alloc_guest(store, alloc_fn, len_u32, sym_len)?;
        memory
            .write(&mut *store, ptr as usize, &sym_bytes[..sym_len])
            .map_err(|_| WasmSandboxError::MemoryAccessOutOfBounds)?;
        (ptr, len_u32)
    } else {
        (0, 0)
    };
    set_meta_fn
        .call(
            &mut *store,
            (
                meta.timeframe_seconds,
                u32::from(meta.price_scale),
                u32::from(meta.quantity_scale),
                flags,
                sym_ptr,
                sym_len_u32,
            ),
        )
        .map_err(|err| classify_wasmi_error(&err, false))
}

fn align_secondary_market_to_primary(
    primary_timestamps: &[i64],
    secondary: &crate::StudyMarketSeries,
    request: StudyRequestContext,
) -> WasmAlignedSecondary {
    let n = primary_timestamps.len();
    let mut open = vec![f64::NAN; n];
    let mut high = vec![f64::NAN; n];
    let mut low = vec![f64::NAN; n];
    let mut close = vec![f64::NAN; n];
    let mut volume = vec![f64::NAN; n];
    let sec_bars = secondary.bars();
    let p_scale = sec_bars.price_scale();
    let q_scale = sec_bars.quantity_scale();
    for (i, &ts) in primary_timestamps.iter().enumerate() {
        let prev_ts = i.checked_sub(1).and_then(|p| primary_timestamps.get(p).copied());
        if let Some(sec_idx) = align_higher_timeframe_bar(ts, prev_ts, secondary, request)
            && let Some(bar) = sec_bars.get(sec_idx)
        {
            open[i] = fixed_point_to_f64(bar.open, p_scale).unwrap_or(f64::NAN);
            high[i] = fixed_point_to_f64(bar.high, p_scale).unwrap_or(f64::NAN);
            low[i] = fixed_point_to_f64(bar.low, p_scale).unwrap_or(f64::NAN);
            close[i] = fixed_point_to_f64(bar.close, p_scale).unwrap_or(f64::NAN);
            volume[i] = fixed_point_to_f64(bar.volume, q_scale).unwrap_or(f64::NAN);
        }
    }
    WasmAlignedSecondary {
        open,
        high,
        low,
        close,
        volume,
    }
}

/// Builds per-bar order-flow columns (`buy_vol`, `sell_vol`, `delta`, `trade_count`, `poc`).
///
/// Uses exact aggressor `Buy`/`Sell` prints from `trades` for every bar covered by the live trade
/// window, and falls back to standard OHLCV intrabar close-location polarity for historical bars
/// outside the live trade window so CVD and Bar Delta remain populated across full chart history.
#[must_use]
pub fn aggregate_order_flow_columns(
    bars: &StudySandboxColumnarBars,
    trades: Option<crate::StudyTradeWindow<'_>>,
) -> WasmOrderFlowColumns {
    let timestamps = &bars.timestamps_unix_nanos;
    let n = timestamps.len();
    let mut buy_vol = vec![0.0; n];
    let mut sell_vol = vec![0.0; n];
    let mut delta = vec![0.0; n];
    let mut trade_count = vec![0.0; n];
    let mut poc = vec![f64::NAN; n];

    if let Some(window) = trades {
        let p_scale = window.price_scale();
        let q_scale = window.quantity_scale();
        let mut per_bar_bins: BTreeMap<usize, BTreeMap<i64, i64>> = BTreeMap::new();
        for sample in window.iter() {
            let idx = timestamps
                .partition_point(|&t| t <= sample.observed_unix_nanos)
                .checked_sub(1);
            let Some(bar_idx) = idx else {
                continue;
            };
            if bar_idx >= n {
                continue;
            }
            let qty = fixed_point_to_f64(sample.quantity, q_scale).unwrap_or(0.0);
            match sample.aggressor {
                AggressorSide::Buy => buy_vol[bar_idx] += qty,
                AggressorSide::Sell => sell_vol[bar_idx] += qty,
                AggressorSide::Unknown => {}
            }
            trade_count[bar_idx] += 1.0;
            let bins = per_bar_bins.entry(bar_idx).or_default();
            if bins.len() < 256 || bins.contains_key(&sample.price) {
                *bins.entry(sample.price).or_insert(0) += sample.quantity.max(0);
            }
        }
        for (bar_idx, bins) in per_bar_bins {
            if let Some((&best_price, _)) = bins.iter().max_by_key(|(_, vol)| **vol) {
                poc[bar_idx] = fixed_point_to_f64(best_price, p_scale).unwrap_or(f64::NAN);
            }
        }
    }

    // For historical bars with no retained live trade prints, estimate buy/sell split from
    // intrabar OHLCV close-location polarity so historical delta & CVD are never flat zero.
    for i in 0..n {
        if trade_count[i] == 0.0 {
            let vol = fixed_point_to_f64(bars.volume[i], bars.quantity_scale).unwrap_or(0.0);
            if vol > 0.0 {
                let o = fixed_point_to_f64(bars.open[i], bars.price_scale).unwrap_or(0.0);
                let h = fixed_point_to_f64(bars.high[i], bars.price_scale).unwrap_or(0.0);
                let l = fixed_point_to_f64(bars.low[i], bars.price_scale).unwrap_or(0.0);
                let c = fixed_point_to_f64(bars.close[i], bars.price_scale).unwrap_or(0.0);
                let range = h - l;
                let buy_ratio = if range > 0.0 {
                    ((c - l) / range).clamp(0.0, 1.0)
                } else if c > o {
                    1.0
                } else if c < o {
                    0.0
                } else {
                    0.5
                };
                buy_vol[i] = vol * buy_ratio;
                sell_vol[i] = vol - buy_vol[i];
                poc[i] = (h + l + c) / 3.0;
            }
        }
        delta[i] = buy_vol[i] - sell_vol[i];
    }

    WasmOrderFlowColumns {
        buy_vol,
        sell_vol,
        delta,
        trade_count,
        poc,
    }
}

fn pack_five_f64_columns(
    c0: &[f64],
    c1: &[f64],
    c2: &[f64],
    c3: &[f64],
    c4: &[f64],
    n: usize,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(n * 5 * size_of::<f64>());
    for col in [c0, c1, c2, c3, c4] {
        for i in 0..n {
            let v = col.get(i).copied().unwrap_or(f64::NAN);
            out.extend_from_slice(&v.to_le_bytes());
        }
    }
    out
}

fn apply_wasm_secondaries(
    instance: wasmi::Instance,
    store: &mut Store<SandboxStoreState>,
    memory: wasmi::Memory,
    alloc_fn: wasmi::TypedFunc<u32, u32>,
    secondaries: &[WasmAlignedSecondary],
    bar_count: usize,
) -> Result<(), WasmSandboxError> {
    if secondaries.is_empty() || bar_count == 0 {
        return Ok(());
    }
    let Ok(set_sec_fn) =
        instance.get_typed_func::<(u32, u32, u32), ()>(&*store, WASM_EXPORT_SET_SECONDARY)
    else {
        return Ok(());
    };
    let count_u32 = u32::try_from(bar_count).unwrap_or(0);
    for (slot, sec) in secondaries.iter().take(MAX_WASM_SECONDARY_SERIES).enumerate() {
        let raw = pack_five_f64_columns(
            &sec.open,
            &sec.high,
            &sec.low,
            &sec.close,
            &sec.volume,
            bar_count,
        );
        let len_u32 = u32::try_from(raw.len()).map_err(|_| WasmSandboxError::AllocationFailed {
            requested_bytes: raw.len(),
        })?;
        let ptr = alloc_guest(store, alloc_fn, len_u32, raw.len())?;
        memory
            .write(&mut *store, ptr as usize, &raw)
            .map_err(|_| WasmSandboxError::MemoryAccessOutOfBounds)?;
        set_sec_fn
            .call(&mut *store, (slot as u32, ptr, count_u32))
            .map_err(|err| classify_wasmi_error(&err, false))?;
    }
    Ok(())
}

fn apply_wasm_order_flow(
    instance: wasmi::Instance,
    store: &mut Store<SandboxStoreState>,
    memory: wasmi::Memory,
    alloc_fn: wasmi::TypedFunc<u32, u32>,
    order_flow: &WasmOrderFlowColumns,
    bar_count: usize,
) -> Result<(), WasmSandboxError> {
    if bar_count == 0 {
        return Ok(());
    }
    let Ok(set_of_fn) =
        instance.get_typed_func::<(u32, u32), ()>(&*store, WASM_EXPORT_SET_ORDER_FLOW)
    else {
        return Ok(());
    };
    let raw = pack_five_f64_columns(
        &order_flow.buy_vol,
        &order_flow.sell_vol,
        &order_flow.delta,
        &order_flow.trade_count,
        &order_flow.poc,
        bar_count,
    );
    let len_u32 = u32::try_from(raw.len()).map_err(|_| WasmSandboxError::AllocationFailed {
        requested_bytes: raw.len(),
    })?;
    let count_u32 = u32::try_from(bar_count).unwrap_or(0);
    let ptr = alloc_guest(store, alloc_fn, len_u32, raw.len())?;
    memory
        .write(&mut *store, ptr as usize, &raw)
        .map_err(|_| WasmSandboxError::MemoryAccessOutOfBounds)?;
    set_of_fn
        .call(&mut *store, (ptr, count_u32))
        .map_err(|err| classify_wasmi_error(&err, false))
}

fn apply_wasm_viewport(
    instance: wasmi::Instance,
    store: &mut Store<SandboxStoreState>,
    viewport: Option<(usize, usize)>,
    bar_count: usize,
) -> Result<(), WasmSandboxError> {
    let Ok(set_vp_fn) =
        instance.get_typed_func::<(u32, u32), ()>(&*store, WASM_EXPORT_SET_VIEWPORT)
    else {
        return Ok(());
    };
    let (left, right) = viewport.unwrap_or((0, bar_count.saturating_sub(1)));
    let left_u32 = u32::try_from(left).unwrap_or(0);
    let right_u32 = u32::try_from(right).unwrap_or(u32::MAX);
    set_vp_fn
        .call(&mut *store, (left_u32, right_u32))
        .map_err(|err| classify_wasmi_error(&err, false))
}

fn read_wasm_scene_buffer(
    instance: wasmi::Instance,
    store: &mut Store<SandboxStoreState>,
    memory: wasmi::Memory,
) -> Result<Vec<u8>, WasmSandboxError> {
    let (Ok(ptr_fn), Ok(len_fn)) = (
        instance.get_typed_func::<(), u32>(&*store, WASM_EXPORT_SCENE_PTR),
        instance.get_typed_func::<(), u32>(&*store, WASM_EXPORT_SCENE_LEN),
    ) else {
        return Ok(Vec::new());
    };
    let len = len_fn
        .call(&mut *store, ())
        .map_err(|err| classify_wasmi_error(&err, false))? as usize;
    if len == 0 {
        return Ok(Vec::new());
    }
    let clamped_len = len.min(MAX_WASM_SCENE_BYTES);
    let ptr = ptr_fn
        .call(&mut *store, ())
        .map_err(|err| classify_wasmi_error(&err, false))? as usize;
    let mut bytes = vec![0_u8; clamped_len];
    memory
        .read(&*store, ptr, &mut bytes)
        .map_err(|_| WasmSandboxError::MemoryAccessOutOfBounds)?;
    Ok(bytes)
}

fn resolve_visible_bar_range(
    timestamps: &[i64],
    visible_start_ns: Option<i64>,
    visible_end_ns: Option<i64>,
) -> Option<(usize, usize)> {
    if timestamps.is_empty() {
        return None;
    }
    let last = timestamps.len() - 1;
    let left = visible_start_ns
        .map_or(0, |ts| timestamps.partition_point(|&t| t < ts))
        .min(last);
    let right = visible_end_ns
        .map_or(last, |ts| {
            timestamps.partition_point(|&t| t <= ts).saturating_sub(1)
        })
        .min(last)
        .max(left);
    Some((left, right))
}

fn estimate_bar_step_ns(timestamps: &[i64]) -> i64 {
    if timestamps.len() < 2 {
        return 60_000_000_000;
    }
    let mut deltas = [0_i64; 9];
    let mut count = 0usize;
    let start = timestamps.len().saturating_sub(10);
    for i in (start + 1)..timestamps.len() {
        let d = timestamps[i].saturating_sub(timestamps[i - 1]);
        if d > 0 {
            deltas[count] = d;
            count += 1;
        }
    }
    if count == 0 {
        return 60_000_000_000;
    }
    deltas[..count].sort_unstable();
    deltas[count / 2].max(1_000_000_000)
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
fn bar_coord_to_unix_nanos(timestamps: &[i64], step_ns: i64, bar_coord: f64) -> Option<i64> {
    if timestamps.is_empty() || !bar_coord.is_finite() {
        return None;
    }
    let last_idx = timestamps.len() - 1;
    if bar_coord <= 0.0 {
        let offset = (bar_coord * (step_ns as f64)).round();
        return offset
            .is_finite()
            .then_some(timestamps[0].saturating_add(offset as i64));
    }
    let last_f = last_idx as f64;
    if bar_coord >= last_f {
        let offset = ((bar_coord - last_f) * (step_ns as f64)).round();
        return offset
            .is_finite()
            .then_some(timestamps[last_idx].saturating_add(offset as i64));
    }
    let idx = bar_coord.floor() as usize;
    let frac = bar_coord - (idx as f64);
    let t0 = timestamps[idx];
    let t1 = timestamps[(idx + 1).min(last_idx)];
    let delta = ((t1.saturating_sub(t0) as f64) * frac).round();
    delta.is_finite().then_some(t0.saturating_add(delta as i64))
}

const fn unpack_study_color(rgba: u32) -> StudyColor {
    let [r, g, b, a] = rgba.to_be_bytes();
    StudyColor::rgba(r, g, b, a)
}

const fn decode_line_style(code: u8) -> StudyLineStyle {
    match code {
        1 => StudyLineStyle::Dashed,
        2 => StudyLineStyle::Dotted,
        _ => StudyLineStyle::Solid,
    }
}

const fn decode_line_extend(code: u8) -> StudyLineExtend {
    match code {
        1 => StudyLineExtend::Right,
        2 => StudyLineExtend::Left,
        3 => StudyLineExtend::Both,
        _ => StudyLineExtend::None,
    }
}

const fn decode_label_style(code: u8) -> StudyLabelStyle {
    match code {
        1 => StudyLabelStyle::LabelDown,
        2 => StudyLabelStyle::LabelLeft,
        3 => StudyLabelStyle::LabelRight,
        4 => StudyLabelStyle::TextOnly,
        5 => StudyLabelStyle::Circle,
        6 => StudyLabelStyle::TriangleUp,
        7 => StudyLabelStyle::TriangleDown,
        8 => StudyLabelStyle::ArrowUp,
        9 => StudyLabelStyle::ArrowDown,
        10 => StudyLabelStyle::Cross,
        _ => StudyLabelStyle::LabelUp,
    }
}

const fn decode_table_position(code: u8) -> StudyTablePosition {
    match code {
        0 => StudyTablePosition::TopLeft,
        1 => StudyTablePosition::TopCenter,
        2 => StudyTablePosition::TopRight,
        3 => StudyTablePosition::MiddleLeft,
        4 => StudyTablePosition::MiddleCenter,
        5 => StudyTablePosition::MiddleRight,
        6 => StudyTablePosition::BottomLeft,
        7 => StudyTablePosition::BottomCenter,
        _ => StudyTablePosition::BottomRight,
    }
}

const fn decode_text_align(code: u8) -> StudyTextAlign {
    match code {
        1 => StudyTextAlign::Center,
        2 => StudyTextAlign::Right,
        _ => StudyTextAlign::Left,
    }
}

struct SceneCursor<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> SceneCursor<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(n)?;
        let slice = self.bytes.get(self.pos..end)?;
        self.pos = end;
        Some(slice)
    }

    fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }

    fn u16(&mut self) -> Option<u16> {
        let b: [u8; 2] = self.take(2)?.try_into().ok()?;
        Some(u16::from_le_bytes(b))
    }

    fn u32(&mut self) -> Option<u32> {
        let b: [u8; 4] = self.take(4)?.try_into().ok()?;
        Some(u32::from_le_bytes(b))
    }

    fn f64(&mut self) -> Option<f64> {
        let b: [u8; 8] = self.take(8)?.try_into().ok()?;
        Some(f64::from_le_bytes(b))
    }

    fn text(&mut self) -> Option<String> {
        let len = usize::from(self.u16()?).min(MAXIMUM_STUDY_SCENE_TEXT_BYTES);
        let raw = self.take(len)?;
        Some(String::from_utf8_lossy(raw).into_owned())
    }
}

struct DecodedWasmScene {
    objects: Vec<StudySceneObject>,
    /// `(first_line_object_index, second_line_object_index, fill_color)` resolved after inserting lines.
    line_fills: Vec<(usize, usize, StudyColor)>,
    bar_colors: Vec<(usize, StudyColor)>,
    bg_colors: Vec<(usize, StudyColor)>,
    /// `(output_index, local_bar_index, color)` per-bar dynamic plot color overrides.
    plot_colors: Vec<(usize, usize, StudyColor)>,
    alerts: Vec<StudyAlertEvent>,
    logs: Vec<WasmStudyLogEntry>,
    strategy_commands: Vec<crate::strategy::StrategyCommand>,
}

fn decode_box_ex_cmd(
    cursor: &mut SceneCursor<'_>,
    timestamps: &[i64],
    step_ns: i64,
    objects: &mut Vec<StudySceneObject>,
) -> bool {
    let (
        Some(x1),
        Some(top),
        Some(x2),
        Some(bot),
        Some(border),
        Some(fill),
        Some(bw),
        Some(bstyle),
        Some(text_rgba),
        Some(text),
    ) = (
        cursor.f64(),
        cursor.f64(),
        cursor.f64(),
        cursor.f64(),
        cursor.u32(),
        cursor.u32(),
        cursor.u8(),
        cursor.u8(),
        cursor.u32(),
        cursor.text(),
    )
    else {
        return false;
    };
    if objects.len() < MAXIMUM_STUDY_SCENE_OBJECTS
        && top.is_finite()
        && bot.is_finite()
        && let (Some(t1), Some(t2)) = (
            bar_coord_to_unix_nanos(timestamps, step_ns, x1),
            bar_coord_to_unix_nanos(timestamps, step_ns, x2),
        )
    {
        let has_text = !text.is_empty();
        objects.push(StudySceneObject::Box(StudyBoxObject {
            top_left: StudyPoint::new(t1, top),
            bottom_right: StudyPoint::new(t2, bot),
            border_color: unpack_study_color(border),
            border_width: bw,
            border_style: decode_line_style(bstyle),
            fill_color: ((fill & 0xFF) != 0).then(|| unpack_study_color(fill)),
            text: has_text.then_some(text),
            text_color: (has_text && (text_rgba & 0xFF) != 0)
                .then(|| unpack_study_color(text_rgba)),
        }));
    }
    true
}

fn decode_plot_shape_cmd(
    cursor: &mut SceneCursor<'_>,
    bars: &StudySandboxColumnarBars,
    objects: &mut Vec<StudySceneObject>,
) -> bool {
    let (
        Some(bar_idx),
        Some(location),
        Some(explicit_price),
        Some(shape),
        Some(color),
        Some(text_color),
        Some(text),
    ) = (
        cursor.u32(),
        cursor.u8(),
        cursor.f64(),
        cursor.u8(),
        cursor.u32(),
        cursor.u32(),
        cursor.text(),
    )
    else {
        return false;
    };
    let idx = bar_idx as usize;
    if objects.len() < MAXIMUM_STUDY_SCENE_OBJECTS
        && let Some(&ts) = bars.timestamps_unix_nanos.get(idx)
    {
        let price = match location {
            0 => bars
                .high
                .get(idx)
                .and_then(|&h| fixed_point_to_f64(h, bars.price_scale)),
            1 => bars
                .low
                .get(idx)
                .and_then(|&l| fixed_point_to_f64(l, bars.price_scale)),
            _ => explicit_price.is_finite().then_some(explicit_price),
        };
        if let Some(y) = price {
            objects.push(StudySceneObject::Label(StudyLabelObject {
                point: StudyPoint::new(ts, y),
                text,
                style: decode_label_style(shape),
                color: unpack_study_color(color),
                text_color: unpack_study_color(text_color),
            }));
        }
    }
    true
}

const fn decode_alert_frequency(code: u8) -> StudyAlertFrequency {
    match code {
        1 => StudyAlertFrequency::OncePerBarClose,
        2 => StudyAlertFrequency::EveryTick,
        _ => StudyAlertFrequency::OncePerBar,
    }
}

const fn decode_log_level(code: u8) -> WasmStudyLogLevel {
    match code {
        1 => WasmStudyLogLevel::Warn,
        2 => WasmStudyLogLevel::Error,
        _ => WasmStudyLogLevel::Info,
    }
}

fn decode_alert_cmd(
    cursor: &mut SceneCursor<'_>,
    timestamps: &[i64],
    alerts: &mut Vec<StudyAlertEvent>,
) -> bool {
    let (Some(bar_idx), Some(freq_code), Some(cond_id), Some(message)) =
        (cursor.u32(), cursor.u8(), cursor.text(), cursor.text())
    else {
        return false;
    };
    if alerts.len() < MAXIMUM_STUDY_ALERTS_PER_WAVE
        && let Some(&ts) = timestamps.get(bar_idx as usize)
    {
        let trimmed_id = cond_id.trim();
        if !trimmed_id.is_empty() && trimmed_id.len() <= MAXIMUM_STUDY_IDENTIFIER_BYTES {
            let mut msg = message;
            if msg.len() > MAXIMUM_STUDY_ALERT_MESSAGE_BYTES {
                let mut end = MAXIMUM_STUDY_ALERT_MESSAGE_BYTES;
                while end > 0 && !msg.is_char_boundary(end) {
                    end -= 1;
                }
                msg.truncate(end);
            }
            alerts.push(StudyAlertEvent {
                condition_id: trimmed_id.to_string(),
                message: msg,
                bar_timestamp_unix_nanos: ts,
                frequency: decode_alert_frequency(freq_code),
            });
        }
    }
    true
}

fn decode_log_cmd(cursor: &mut SceneCursor<'_>, logs: &mut Vec<WasmStudyLogEntry>) -> bool {
    let (Some(level_code), Some(mut message)) = (cursor.u8(), cursor.text()) else {
        return false;
    };
    if logs.len() < MAX_WASM_LOGS_PER_WAVE {
        if message.len() > MAX_WASM_LOG_MESSAGE_BYTES {
            let mut end = MAX_WASM_LOG_MESSAGE_BYTES;
            while end > 0 && !message.is_char_boundary(end) {
                end -= 1;
            }
            message.truncate(end);
        }
        logs.push(WasmStudyLogEntry {
            level: decode_log_level(level_code),
            message,
        });
    }
    true
}

fn decode_box_cmd(
    cursor: &mut SceneCursor<'_>,
    timestamps: &[i64],
    step_ns: i64,
    objects: &mut Vec<StudySceneObject>,
) -> bool {
    let (Some(x1), Some(top), Some(x2), Some(bot), Some(border), Some(fill), Some(bw)) = (
        cursor.f64(),
        cursor.f64(),
        cursor.f64(),
        cursor.f64(),
        cursor.u32(),
        cursor.u32(),
        cursor.u8(),
    ) else {
        return false;
    };
    if objects.len() < MAXIMUM_STUDY_SCENE_OBJECTS
        && top.is_finite()
        && bot.is_finite()
        && let (Some(t1), Some(t2)) = (
            bar_coord_to_unix_nanos(timestamps, step_ns, x1),
            bar_coord_to_unix_nanos(timestamps, step_ns, x2),
        )
    {
        objects.push(StudySceneObject::Box(StudyBoxObject {
            top_left: StudyPoint::new(t1, top),
            bottom_right: StudyPoint::new(t2, bot),
            border_color: unpack_study_color(border),
            border_width: bw,
            border_style: StudyLineStyle::Solid,
            fill_color: ((fill & 0xFF) != 0).then(|| unpack_study_color(fill)),
            text: None,
            text_color: None,
        }));
    }
    true
}

fn decode_line_cmd(
    cursor: &mut SceneCursor<'_>,
    timestamps: &[i64],
    step_ns: i64,
    objects: &mut Vec<StudySceneObject>,
) -> bool {
    let (
        Some(x1),
        Some(y1),
        Some(x2),
        Some(y2),
        Some(color),
        Some(width),
        Some(style),
        Some(extend),
    ) = (
        cursor.f64(),
        cursor.f64(),
        cursor.f64(),
        cursor.f64(),
        cursor.u32(),
        cursor.u8(),
        cursor.u8(),
        cursor.u8(),
    )
    else {
        return false;
    };
    if objects.len() < MAXIMUM_STUDY_SCENE_OBJECTS
        && y1.is_finite()
        && y2.is_finite()
        && let (Some(t1), Some(t2)) = (
            bar_coord_to_unix_nanos(timestamps, step_ns, x1),
            bar_coord_to_unix_nanos(timestamps, step_ns, x2),
        )
    {
        objects.push(StudySceneObject::Line(StudyLineObject {
            start: StudyPoint::new(t1, y1),
            end: StudyPoint::new(t2, y2),
            color: unpack_study_color(color),
            width: width.max(1),
            style: decode_line_style(style),
            extend: decode_line_extend(extend),
        }));
    }
    true
}

fn decode_polyline_cmd(
    cursor: &mut SceneCursor<'_>,
    timestamps: &[i64],
    step_ns: i64,
    objects: &mut Vec<StudySceneObject>,
) -> bool {
    let (Some(count), Some(color), Some(width), Some(closed)) =
        (cursor.u16(), cursor.u32(), cursor.u8(), cursor.u8())
    else {
        return false;
    };
    let count = usize::from(count).min(MAXIMUM_STUDY_POLYLINE_POINTS);
    let mut pts = Vec::with_capacity(count);
    for _ in 0..count {
        let (Some(x), Some(y)) = (cursor.f64(), cursor.f64()) else {
            return false;
        };
        if y.is_finite()
            && let Some(t) = bar_coord_to_unix_nanos(timestamps, step_ns, x)
        {
            pts.push(StudyPoint::new(t, y));
        }
    }
    if pts.len() >= 2 && objects.len() < MAXIMUM_STUDY_SCENE_OBJECTS {
        objects.push(StudySceneObject::Polyline(StudyPolylineObject {
            points: pts,
            color: unpack_study_color(color),
            width: width.max(1),
            style: StudyLineStyle::Solid,
            fill_color: None,
            closed: closed != 0,
        }));
    }
    true
}

fn decode_label_cmd(
    cursor: &mut SceneCursor<'_>,
    timestamps: &[i64],
    step_ns: i64,
    objects: &mut Vec<StudySceneObject>,
) -> bool {
    let (Some(x), Some(y), Some(bg), Some(fg), Some(style), Some(text)) = (
        cursor.f64(),
        cursor.f64(),
        cursor.u32(),
        cursor.u32(),
        cursor.u8(),
        cursor.text(),
    ) else {
        return false;
    };
    if objects.len() < MAXIMUM_STUDY_SCENE_OBJECTS
        && y.is_finite()
        && let Some(t) = bar_coord_to_unix_nanos(timestamps, step_ns, x)
    {
        objects.push(StudySceneObject::Label(StudyLabelObject {
            point: StudyPoint::new(t, y),
            text,
            style: decode_label_style(style),
            color: unpack_study_color(bg),
            text_color: unpack_study_color(fg),
        }));
    }
    true
}

fn decode_table_cell_cmd(
    cursor: &mut SceneCursor<'_>,
    tables: &mut BTreeMap<(u8, u16, u16), StudyTableObject>,
) -> bool {
    let (
        Some(pos),
        Some(cols),
        Some(rows),
        Some(col),
        Some(row),
        Some(fg),
        Some(bg),
        Some(align),
        Some(text),
    ) = (
        cursor.u8(),
        cursor.u16(),
        cursor.u16(),
        cursor.u16(),
        cursor.u16(),
        cursor.u32(),
        cursor.u32(),
        cursor.u8(),
        cursor.text(),
    )
    else {
        return false;
    };
    if cols > 0 && rows > 0 && col < cols && row < rows {
        let entry = tables
            .entry((pos, cols, rows))
            .or_insert_with(|| StudyTableObject {
                position: decode_table_position(pos),
                columns: cols,
                rows,
                border_color: Some(StudyColor::rgba(148, 163, 184, 72)),
                border_width: 1,
                cells: BTreeMap::new(),
            });
        if entry.cells.len() < MAXIMUM_STUDY_TABLE_CELLS {
            entry.cells.insert(
                (col, row),
                StudyTableCell {
                    text,
                    text_color: ((fg & 0xFF) != 0).then(|| unpack_study_color(fg)),
                    background_color: ((bg & 0xFF) != 0).then(|| unpack_study_color(bg)),
                    align: decode_text_align(align),
                },
            );
        }
    }
    true
}

fn decode_wasm_scene_commands(bytes: &[u8], bars: &StudySandboxColumnarBars) -> DecodedWasmScene {
    let timestamps = &bars.timestamps_unix_nanos;
    let step_ns = estimate_bar_step_ns(timestamps);
    let mut cursor = SceneCursor { bytes, pos: 0 };
    let mut objects: Vec<StudySceneObject> = Vec::new();
    let mut line_obj_indices: Vec<usize> = Vec::new();
    let mut line_fills: Vec<(usize, usize, StudyColor)> = Vec::new();
    let mut tables: BTreeMap<(u8, u16, u16), StudyTableObject> = BTreeMap::new();
    let mut bar_colors: Vec<(usize, StudyColor)> = Vec::new();
    let mut bg_colors: Vec<(usize, StudyColor)> = Vec::new();
    let mut plot_colors: Vec<(usize, usize, StudyColor)> = Vec::new();
    let mut alerts: Vec<StudyAlertEvent> = Vec::new();
    let mut logs: Vec<WasmStudyLogEntry> = Vec::new();
    let mut strategy_commands: Vec<crate::strategy::StrategyCommand> = Vec::new();

    while let Some(op) = cursor.u8() {
        let ok = match op {
            OP_BOX => decode_box_cmd(&mut cursor, timestamps, step_ns, &mut objects),
            OP_BOX_EX => decode_box_ex_cmd(&mut cursor, timestamps, step_ns, &mut objects),
            OP_LINE => {
                let prev_len = objects.len();
                let res = decode_line_cmd(&mut cursor, timestamps, step_ns, &mut objects);
                if res && objects.len() > prev_len {
                    line_obj_indices.push(prev_len);
                }
                res
            }
            OP_LINE_FILL => match (cursor.u32(), cursor.u32(), cursor.u32()) {
                (Some(a), Some(b), Some(color)) => {
                    if let (Some(&idx_a), Some(&idx_b)) = (
                        line_obj_indices.get(a as usize),
                        line_obj_indices.get(b as usize),
                    ) && idx_a != idx_b
                    {
                        line_fills.push((idx_a, idx_b, unpack_study_color(color)));
                    }
                    true
                }
                _ => false,
            },
            OP_POLYLINE => decode_polyline_cmd(&mut cursor, timestamps, step_ns, &mut objects),
            OP_LABEL => decode_label_cmd(&mut cursor, timestamps, step_ns, &mut objects),
            OP_PLOT_SHAPE => decode_plot_shape_cmd(&mut cursor, bars, &mut objects),
            OP_TABLE_CELL => decode_table_cell_cmd(&mut cursor, &mut tables),
            OP_BAR_COLOR => match (cursor.u32(), cursor.u32()) {
                (Some(idx), Some(color)) => {
                    bar_colors.push((idx as usize, unpack_study_color(color)));
                    true
                }
                _ => false,
            },
            OP_BG_COLOR => match (cursor.u32(), cursor.u32()) {
                (Some(idx), Some(color)) => {
                    bg_colors.push((idx as usize, unpack_study_color(color)));
                    true
                }
                _ => false,
            },
            OP_PLOT_COLOR => match (cursor.u8(), cursor.u32(), cursor.u32()) {
                (Some(out_idx), Some(bar_idx), Some(color)) => {
                    plot_colors.push((
                        usize::from(out_idx),
                        bar_idx as usize,
                        unpack_study_color(color),
                    ));
                    true
                }
                _ => false,
            },
            OP_ALERT => decode_alert_cmd(&mut cursor, timestamps, &mut alerts),
            OP_LOG => decode_log_cmd(&mut cursor, &mut logs),
            OP_STRATEGY_ENTRY => match (
                cursor.u32(),
                cursor.u8(),
                cursor.f64(),
                cursor.f64(),
                cursor.text(),
            ) {
                (Some(bar_idx), Some(dir), Some(qty), Some(limit), Some(id)) => {
                    if strategy_commands.len() < crate::strategy::MAX_STRATEGY_TRADES * 2 {
                        let direction = if dir == 0 {
                            crate::strategy::StrategyDirection::Long
                        } else {
                            crate::strategy::StrategyDirection::Short
                        };
                        strategy_commands.push(crate::strategy::StrategyCommand::Entry {
                            bar_index: bar_idx as usize,
                            id,
                            direction,
                            qty,
                            limit_price: limit.is_finite().then_some(limit),
                        });
                    }
                    true
                }
                _ => false,
            },
            OP_STRATEGY_EXIT => match (
                cursor.u32(),
                cursor.f64(),
                cursor.f64(),
                cursor.text(),
                cursor.text(),
            ) {
                (Some(bar_idx), Some(tp), Some(sl), Some(id), Some(from_entry)) => {
                    if strategy_commands.len() < crate::strategy::MAX_STRATEGY_TRADES * 2 {
                        strategy_commands.push(crate::strategy::StrategyCommand::Exit {
                            bar_index: bar_idx as usize,
                            id,
                            from_entry,
                            profit_price: tp.is_finite().then_some(tp),
                            loss_price: sl.is_finite().then_some(sl),
                        });
                    }
                    true
                }
                _ => false,
            },
            OP_STRATEGY_CLOSE => match (cursor.u32(), cursor.text()) {
                (Some(bar_idx), Some(id)) => {
                    if strategy_commands.len() < crate::strategy::MAX_STRATEGY_TRADES * 2 {
                        strategy_commands.push(crate::strategy::StrategyCommand::Close {
                            bar_index: bar_idx as usize,
                            id,
                        });
                    }
                    true
                }
                _ => false,
            },
            OP_STRATEGY_CLOSE_ALL => match cursor.u32() {
                Some(bar_idx) => {
                    if strategy_commands.len() < crate::strategy::MAX_STRATEGY_TRADES * 2 {
                        strategy_commands.push(crate::strategy::StrategyCommand::CloseAll {
                            bar_index: bar_idx as usize,
                        });
                    }
                    true
                }
                None => false,
            },
            _ => false,
        };
        if !ok {
            break;
        }
    }

    // Auto-emit crisp chart entry/exit markers for strategy trades when room remains in scene.
    if !strategy_commands.is_empty() {
        let tick_size = 10_f64.powi(-i32::from(bars.price_scale));
        let report = crate::strategy::evaluate_strategy(
            bars,
            &strategy_commands,
            crate::strategy::StrategyConfig {
                tick_size,
                ..crate::strategy::StrategyConfig::default()
            },
        );
        for trade in report.trades.iter().rev().take(128).rev() {
            if objects.len() + 2 > MAXIMUM_STUDY_SCENE_OBJECTS {
                break;
            }
            let (entry_style, entry_color, entry_label) = match trade.direction {
                crate::strategy::StrategyDirection::Long => (
                    StudyLabelStyle::ArrowUp,
                    StudyColor::rgb(34, 197, 94),
                    format!("L {}", trade.entry_id),
                ),
                crate::strategy::StrategyDirection::Short => (
                    StudyLabelStyle::ArrowDown,
                    StudyColor::rgb(239, 68, 68),
                    format!("S {}", trade.entry_id),
                ),
            };
            objects.push(StudySceneObject::Label(StudyLabelObject {
                point: StudyPoint::new(trade.entry_timestamp_ns, trade.entry_price),
                text: entry_label,
                style: entry_style,
                color: entry_color,
                text_color: StudyColor::rgb(255, 255, 255),
            }));
            let exit_color = if trade.net_pnl >= 0.0 {
                StudyColor::rgb(34, 197, 94)
            } else {
                StudyColor::rgb(239, 68, 68)
            };
            objects.push(StudySceneObject::Label(StudyLabelObject {
                point: StudyPoint::new(trade.exit_timestamp_ns, trade.exit_price),
                text: format!("{:+.2}", trade.net_pnl),
                style: StudyLabelStyle::LabelDown,
                color: exit_color,
                text_color: StudyColor::rgb(255, 255, 255),
            }));
        }
    }

    for table in tables.into_values() {
        if objects.len() < MAXIMUM_STUDY_SCENE_OBJECTS {
            objects.push(StudySceneObject::Table(table));
        }
    }
    DecodedWasmScene {
        objects,
        line_fills,
        bar_colors,
        bg_colors,
        plot_colors,
        alerts,
        logs,
        strategy_commands,
    }
}

fn reconcile_wasm_scene(
    scene: &mut StudySceneState,
    objects: Vec<StudySceneObject>,
    line_fills: &[(usize, usize, StudyColor)],
) -> Result<(), String> {
    let existing_ids: Vec<_> = scene.iter().map(|(id, _)| id).collect();
    let keep = existing_ids.len().min(objects.len());
    let mut assigned_ids = Vec::with_capacity(objects.len() + line_fills.len());
    let mut iter = objects.into_iter();
    for &id in &existing_ids[..keep] {
        if let Some(next_obj) = iter.next() {
            if scene.get(id) != Some(&next_obj) {
                scene.update(id, next_obj)?;
            }
            assigned_ids.push(id);
        }
    }
    for extra in iter {
        let id = scene.insert(extra)?;
        assigned_ids.push(id);
    }
    let mut next_slot = keep;
    for &(idx_a, idx_b, color) in line_fills {
        if let (Some(&first_line), Some(&second_line)) =
            (assigned_ids.get(idx_a), assigned_ids.get(idx_b))
        {
            let fill_obj = StudySceneObject::LineFill(StudyLineFillObject {
                first_line,
                second_line,
                color,
            });
            if let Some(&existing_id) = existing_ids.get(next_slot) {
                if scene.get(existing_id) != Some(&fill_obj) {
                    scene.update(existing_id, fill_obj)?;
                }
                next_slot += 1;
            } else if scene.len() < MAXIMUM_STUDY_SCENE_OBJECTS {
                let _ = scene.insert(fill_obj)?;
            }
        }
    }
    for &stale_id in &existing_ids[next_slot..] {
        let _ = scene.delete(stale_id);
    }
    Ok(())
}

fn apply_decoded_wasm_scene(
    context: &mut StudyExecutionContext<'_>,
    scene_bytes: &[u8],
    bars: &StudySandboxColumnarBars,
    slice_start: usize,
    write_from: usize,
    write_to: usize,
) -> Result<(), String> {
    let decoded = decode_wasm_scene_commands(scene_bytes, bars);
    reconcile_wasm_scene(context.scene_mut(), decoded.objects, &decoded.line_fills)?;
    for (local_idx, color) in decoded.bar_colors {
        let row = slice_start.saturating_add(local_idx);
        if row >= write_from && row < write_to {
            context.set_bar_color(row, (color.a != 0).then_some(color))?;
        }
    }
    for (local_idx, color) in decoded.bg_colors {
        let row = slice_start.saturating_add(local_idx);
        if row >= write_from && row < write_to {
            context.set_background_color(row, (color.a != 0).then_some(color))?;
        }
    }
    for (out_idx, local_idx, color) in decoded.plot_colors {
        let row = slice_start.saturating_add(local_idx);
        if row >= write_from
            && row < write_to
            && let Some(output) = context.output(out_idx)
        {
            output.set_point_color(row, (color.a != 0).then_some(color))?;
        }
    }
    for alert in decoded.alerts {
        context.emit_alert(alert)?;
    }
    Ok(())
}

fn checked_wasm_sizes(
    input_bytes: usize,
    out_bytes: usize,
    bar_count: usize,
    output_count: usize,
    limits: StudySandboxLimits,
) -> Result<(u32, u32, u32, u32), WasmSandboxError> {
    let input_len_u32 =
        u32::try_from(input_bytes).map_err(|_| WasmSandboxError::AllocationFailed {
            requested_bytes: input_bytes,
        })?;
    let out_len_u32 = u32::try_from(out_bytes).map_err(|_| WasmSandboxError::AllocationFailed {
        requested_bytes: out_bytes,
    })?;
    let bar_count_u32 = u32::try_from(bar_count).map_err(|_| WasmSandboxError::InputTooLarge {
        maximum: limits.max_output_points,
        actual: bar_count,
    })?;
    let out_count_u32 = u32::try_from(output_count).unwrap_or(1);
    Ok((input_len_u32, out_len_u32, bar_count_u32, out_count_u32))
}

fn alloc_guest(
    store: &mut Store<SandboxStoreState>,
    alloc_fn: wasmi::TypedFunc<u32, u32>,
    byte_len_u32: u32,
    requested_bytes: usize,
) -> Result<u32, WasmSandboxError> {
    let ptr = alloc_fn
        .call(store, byte_len_u32)
        .map_err(|err| classify_wasmi_error(&err, false))?;
    if ptr == 0 {
        return Err(WasmSandboxError::AllocationFailed { requested_bytes });
    }
    Ok(ptr)
}

fn decode_output_f64_values(
    raw_output: &[u8],
    bar_count: usize,
) -> Result<Vec<Option<f64>>, WasmSandboxError> {
    let mut values = Vec::with_capacity(bar_count);
    for (index, &bytes) in raw_output.as_chunks::<8>().0.iter().enumerate() {
        let value = f64::from_le_bytes(bytes);
        if value.is_nan() {
            values.push(None);
        } else if value.is_finite() {
            values.push(Some(value));
        } else {
            return Err(WasmSandboxError::InvalidOutputValue { index });
        }
    }
    Ok(values)
}

fn pack_columnar_ohlcv_f64(bars: &StudySandboxColumnarBars) -> Vec<u8> {
    let n = bars.len();
    let mut out = Vec::with_capacity(n * INPUT_COLUMN_COUNT * size_of::<f64>());
    let push_col = |out: &mut Vec<u8>, col: &[i64], scale: u8| {
        for &raw in col {
            let val = fixed_point_to_f64(raw, scale).unwrap_or(f64::NAN);
            out.extend_from_slice(&val.to_le_bytes());
        }
    };
    push_col(&mut out, &bars.open, bars.price_scale);
    push_col(&mut out, &bars.high, bars.price_scale);
    push_col(&mut out, &bars.low, bars.price_scale);
    push_col(&mut out, &bars.close, bars.price_scale);
    push_col(&mut out, &bars.volume, bars.quantity_scale);
    out
}

fn classify_wasmi_error(err: &wasmi::Error, is_instantiation: bool) -> WasmSandboxError {
    if let Some(trap_code) = err.as_trap_code() {
        return match trap_code {
            TrapCode::OutOfFuel => WasmSandboxError::FuelExhausted,
            other => WasmSandboxError::GuestTrap(format!("{other:?}")),
        };
    }
    let message = err.to_string();
    let lower = message.to_ascii_lowercase();
    if lower.contains("out of fuel") {
        return WasmSandboxError::FuelExhausted;
    }
    if lower.contains("memory") && (lower.contains("limit") || lower.contains("grow")) {
        return WasmSandboxError::MemoryLimitExceeded;
    }
    if is_instantiation {
        WasmSandboxError::InstantiationFailed(message)
    } else {
        WasmSandboxError::GuestTrap(message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::toolchain::{PreviewFixturePreset, StudyPreviewFixture};

    /// Valid ABI v1 module that computes `(high + low) / 2.0` (HL2 midpoint) for every bar.
    const VALID_HL2_WAT: &str = r#"
        (module
            (memory (export "memory") 2)
            (global $heap (mut i32) (i32.const 1024))
            (func (export "aeris_study_abi_version") (result i32)
                (i32.const 1)
            )
            (func (export "aeris_study_alloc") (param $len i32) (result i32)
                (local $ptr i32)
                (local.set $ptr (global.get $heap))
                (global.set $heap (i32.add (local.get $ptr) (local.get $len)))
                (local.get $ptr)
            )
            (func (export "aeris_study_calculate")
                (param $bars i32) (param $n i32) (param $out i32) (result i32)
                (local $i i32)
                (local $high_base i32)
                (local $low_base i32)
                (local $off i32)
                (local $h f64)
                (local $l f64)
                ;; high is column 1, low is column 2 (each column = n * 8 bytes)
                (local.set $high_base (i32.add (local.get $bars) (i32.mul (local.get $n) (i32.const 8))))
                (local.set $low_base  (i32.add (local.get $bars) (i32.mul (local.get $n) (i32.const 16))))
                (local.set $i (i32.const 0))
                (block $done
                    (loop $loop
                        (br_if $done (i32.ge_u (local.get $i) (local.get $n)))
                        (local.set $off (i32.mul (local.get $i) (i32.const 8)))
                        (local.set $h (f64.load (i32.add (local.get $high_base) (local.get $off))))
                        (local.set $l (f64.load (i32.add (local.get $low_base)  (local.get $off))))
                        (f64.store
                            (i32.add (local.get $out) (local.get $off))
                            (f64.div (f64.add (local.get $h) (local.get $l)) (f64.const 2.0))
                        )
                        (local.set $i (i32.add (local.get $i) (i32.const 1)))
                        (br $loop)
                    )
                )
                (i32.const 0)
            )
        )
    "#;

    /// Adversarial module with an infinite loop inside `aeris_study_calculate`.
    const INFINITE_LOOP_WAT: &str = r#"
        (module
            (memory (export "memory") 2)
            (global $heap (mut i32) (i32.const 1024))
            (func (export "aeris_study_abi_version") (result i32) (i32.const 1))
            (func (export "aeris_study_alloc") (param $len i32) (result i32)
                (local $ptr i32)
                (local.set $ptr (global.get $heap))
                (global.set $heap (i32.add (local.get $ptr) (local.get $len)))
                (local.get $ptr)
            )
            (func (export "aeris_study_calculate")
                (param $bars i32) (param $n i32) (param $out i32) (result i32)
                (loop $forever
                    (br $forever)
                )
                (i32.const 0)
            )
        )
    "#;

    /// Adversarial module declaring a host import (`wasi_snapshot_preview1.fd_write`).
    const DISALLOWED_IMPORT_WAT: &str = r#"
        (module
            (import "wasi_snapshot_preview1" "fd_write" (func $fd_write (param i32)))
            (memory (export "memory") 1)
            (func (export "aeris_study_abi_version") (result i32) (i32.const 1))
            (func (export "aeris_study_alloc") (param i32) (result i32) (i32.const 1024))
            (func (export "aeris_study_calculate") (param i32 i32 i32) (result i32) (i32.const 0))
        )
    "#;

    /// Module reporting an unsupported ABI version (99).
    const WRONG_ABI_WAT: &str = r#"
        (module
            (memory (export "memory") 1)
            (func (export "aeris_study_abi_version") (result i32) (i32.const 99))
            (func (export "aeris_study_alloc") (param i32) (result i32) (i32.const 1024))
            (func (export "aeris_study_calculate") (param i32 i32 i32) (result i32) (i32.const 0))
        )
    "#;

    /// Adversarial module attempting to grow memory past the host limit.
    const MEMORY_BOMB_WAT: &str = r#"
        (module
            (memory (export "memory") 1)
            (func (export "aeris_study_abi_version") (result i32) (i32.const 1))
            (func (export "aeris_study_alloc") (param $len i32) (result i32)
                ;; Try to grow by 512 pages (32 MiB) > 16 MiB limit; return 0 on failure.
                (if (i32.eq (memory.grow (i32.const 512)) (i32.const -1))
                    (then (return (i32.const 0)))
                )
                (i32.const 1024)
            )
            (func (export "aeris_study_calculate") (param i32 i32 i32) (result i32) (i32.const 0))
        )
    "#;

    #[test]
    fn valid_wasm_study_computes_exact_hl2_and_meters_fuel() {
        let limits = StudySandboxLimits::default();
        let compiled = CompiledWasmStudy::from_bytes(VALID_HL2_WAT.as_bytes(), limits)
            .expect("valid WAT compiles and passes ABI v1 check");
        let fixture = StudyPreviewFixture::generate(PreviewFixturePreset::Uptrend, 30, 60);

        let result = compiled
            .execute_columnar(&fixture.bars, limits)
            .expect("valid wasm study executes cleanly");
        assert_eq!(result.values.len(), 30);
        assert!(result.fuel_consumed > 0);
        assert!(result.fuel_consumed < limits.max_instruction_fuel);

        for (i, val) in result.values.iter().enumerate() {
            let expected_h = fixed_point_to_f64(fixture.bars.high[i], fixture.bars.price_scale)
                .expect("finite high");
            let expected_l = fixed_point_to_f64(fixture.bars.low[i], fixture.bars.price_scale)
                .expect("finite low");
            let expected = f64::midpoint(expected_h, expected_l);
            let actual = val.expect("non-gap value");
            assert!(
                (actual - expected).abs() < 1e-9,
                "bar {i}: expected {expected}, got {actual}"
            );
        }
    }

    #[test]
    fn infinite_loop_wasm_traps_deterministically_on_fuel_exhaustion() {
        let limits = StudySandboxLimits {
            max_instruction_fuel: 50_000,
            ..StudySandboxLimits::default()
        };
        let compiled = CompiledWasmStudy::from_bytes(INFINITE_LOOP_WAT.as_bytes(), limits)
            .expect("module compiles");
        let fixture = StudyPreviewFixture::generate(PreviewFixturePreset::Uptrend, 10, 60);

        let err = compiled
            .execute_columnar(&fixture.bars, limits)
            .expect_err("infinite loop must trap on fuel");
        assert_eq!(err, WasmSandboxError::FuelExhausted);
    }

    #[test]
    fn disallowed_host_imports_are_rejected_at_compile_time() {
        let limits = StudySandboxLimits::default();
        let err = CompiledWasmStudy::from_bytes(DISALLOWED_IMPORT_WAT.as_bytes(), limits)
            .expect_err("module with host imports must be denied");
        assert_eq!(
            err,
            WasmSandboxError::ImportDenied {
                module: "wasi_snapshot_preview1".to_string(),
                name: "fd_write".to_string(),
            }
        );
    }

    #[test]
    fn wrong_abi_version_is_rejected_at_validation() {
        let limits = StudySandboxLimits::default();
        let err = CompiledWasmStudy::from_bytes(WRONG_ABI_WAT.as_bytes(), limits)
            .expect_err("mismatched ABI version must be rejected");
        assert_eq!(
            err,
            WasmSandboxError::AbiVersionMismatch {
                expected: STUDY_SANDBOX_ABI_VERSION,
                actual: 99,
            }
        );
    }

    #[test]
    fn excessive_memory_growth_is_blocked_by_store_limits() {
        let limits = StudySandboxLimits::default();
        let compiled = CompiledWasmStudy::from_bytes(MEMORY_BOMB_WAT.as_bytes(), limits)
            .expect("module compiles");
        let fixture = StudyPreviewFixture::generate(PreviewFixturePreset::Uptrend, 5, 60);

        let err = compiled
            .execute_columnar(&fixture.bars, limits)
            .expect_err("32 MiB growth must fail against 16 MiB store limit");
        assert!(matches!(err, WasmSandboxError::AllocationFailed { .. }));
    }

    #[test]
    fn scene_decoder_parses_bounded_alerts_and_logs() {
        let bars = StudySandboxColumnarBars {
            price_scale: 2,
            quantity_scale: 0,
            timestamps_unix_nanos: vec![1_000_000_000_i64, 2_000_000_000, 3_000_000_000],
            open: vec![10000, 10100, 10200],
            high: vec![10500, 10600, 10700],
            low: vec![9900, 10000, 10100],
            close: vec![10100, 10200, 10300],
            volume: vec![10, 20, 30],
        };
        let mut buf = Vec::new();

        // Emit 20 alerts (> MAXIMUM_STUDY_ALERTS_PER_WAVE = 16) to verify hard cap.
        for i in 0..20_u32 {
            let cond = format!("cond_{i}");
            let msg = format!("Alert #{i}");
            buf.push(OP_ALERT);
            buf.extend_from_slice(&(i % 3).to_le_bytes());
            buf.push((i % 3) as u8);
            buf.extend_from_slice(&(cond.len() as u16).to_le_bytes());
            buf.extend_from_slice(cond.as_bytes());
            buf.extend_from_slice(&(msg.len() as u16).to_le_bytes());
            buf.extend_from_slice(msg.as_bytes());
        }

        // Emit 3 logs across all severity levels.
        for (level, text) in [(0_u8, "hello"), (1_u8, "careful"), (2_u8, "boom")] {
            buf.push(OP_LOG);
            buf.push(level);
            buf.extend_from_slice(&(text.len() as u16).to_le_bytes());
            buf.extend_from_slice(text.as_bytes());
        }

        let decoded = decode_wasm_scene_commands(&buf, &bars);
        assert_eq!(decoded.alerts.len(), MAXIMUM_STUDY_ALERTS_PER_WAVE);
        assert_eq!(decoded.alerts[0].condition_id, "cond_0");
        assert_eq!(decoded.alerts[0].bar_timestamp_unix_nanos, 1_000_000_000);
        assert_eq!(decoded.alerts[0].frequency, StudyAlertFrequency::OncePerBar);
        assert_eq!(
            decoded.alerts[1].frequency,
            StudyAlertFrequency::OncePerBarClose
        );
        assert_eq!(decoded.alerts[2].frequency, StudyAlertFrequency::EveryTick);

        assert_eq!(decoded.logs.len(), 3);
        assert_eq!(decoded.logs[0].level, WasmStudyLogLevel::Info);
        assert_eq!(decoded.logs[0].message, "hello");
        assert_eq!(decoded.logs[1].level, WasmStudyLogLevel::Warn);
        assert_eq!(decoded.logs[2].level, WasmStudyLogLevel::Error);
    }

    #[test]
    fn phase_2_scene_decoder_handles_box_text_line_fill_plot_color_and_shapes() {
        let bars = StudySandboxColumnarBars {
            price_scale: 2,
            quantity_scale: 0,
            timestamps_unix_nanos: vec![1_000_000_000_i64, 2_000_000_000, 3_000_000_000],
            open: vec![10000, 10100, 10200],
            high: vec![10500, 10600, 10700],
            low: vec![9900, 10000, 10100],
            close: vec![10100, 10200, 10300],
            volume: vec![10, 20, 30],
        };
        let mut buf = Vec::new();
        // OP_BOX_EX with text & dashed border
        buf.push(OP_BOX_EX);
        buf.extend_from_slice(&0.0_f64.to_le_bytes());
        buf.extend_from_slice(&105.0_f64.to_le_bytes());
        buf.extend_from_slice(&2.0_f64.to_le_bytes());
        buf.extend_from_slice(&100.0_f64.to_le_bytes());
        buf.extend_from_slice(&0x22c5_5eff_u32.to_le_bytes());
        buf.extend_from_slice(&0x22c5_5e40_u32.to_le_bytes());
        buf.push(1);
        buf.push(1); // dashed
        buf.extend_from_slice(&0xffff_ffff_u32.to_le_bytes());
        buf.extend_from_slice(&3_u16.to_le_bytes());
        buf.extend_from_slice(b"FVG");

        // Two OP_LINEs + OP_LINE_FILL between them
        for y in [105.0_f64, 100.0_f64] {
            buf.push(OP_LINE);
            buf.extend_from_slice(&0.0_f64.to_le_bytes());
            buf.extend_from_slice(&y.to_le_bytes());
            buf.extend_from_slice(&2.0_f64.to_le_bytes());
            buf.extend_from_slice(&y.to_le_bytes());
            buf.extend_from_slice(&0x38bd_f8ff_u32.to_le_bytes());
            buf.push(1);
            buf.push(0);
            buf.push(0);
        }
        buf.push(OP_LINE_FILL);
        buf.extend_from_slice(&0_u32.to_le_bytes());
        buf.extend_from_slice(&1_u32.to_le_bytes());
        buf.extend_from_slice(&0x38bd_f830_u32.to_le_bytes());

        // OP_PLOT_COLOR on output 0 bar 2
        buf.push(OP_PLOT_COLOR);
        buf.push(0);
        buf.extend_from_slice(&2_u32.to_le_bytes());
        buf.extend_from_slice(&0xef44_44ff_u32.to_le_bytes());

        // OP_PLOT_SHAPE AboveBar with TriangleDown (7)
        buf.push(OP_PLOT_SHAPE);
        buf.extend_from_slice(&1_u32.to_le_bytes());
        buf.push(0); // AboveBar -> bar.high (106.0)
        buf.extend_from_slice(&0.0_f64.to_le_bytes());
        buf.push(7); // TriangleDown
        buf.extend_from_slice(&0xef44_44ff_u32.to_le_bytes());
        buf.extend_from_slice(&0xffff_ffff_u32.to_le_bytes());
        buf.extend_from_slice(&4_u16.to_le_bytes());
        buf.extend_from_slice(b"SELL");

        let decoded = decode_wasm_scene_commands(&buf, &bars);
        assert_eq!(decoded.objects.len(), 4); // box + 2 lines + shape
        assert_eq!(decoded.line_fills.len(), 1);
        assert_eq!(decoded.plot_colors.len(), 1);
        assert_eq!(decoded.plot_colors[0].0, 0);
        assert_eq!(decoded.plot_colors[0].1, 2);

        if let StudySceneObject::Box(bx) = &decoded.objects[0] {
            assert_eq!(bx.text.as_deref(), Some("FVG"));
            assert_eq!(bx.border_style, StudyLineStyle::Dashed);
        } else {
            panic!("expected Box at index 0");
        }
        if let StudySceneObject::Label(lbl) = &decoded.objects[3] {
            assert_eq!(lbl.style, StudyLabelStyle::TriangleDown);
            assert!((lbl.point.price - 106.0).abs() < 1e-9);
            assert_eq!(lbl.text, "SELL");
        } else {
            panic!("expected Label shape at index 3");
        }
    }
}
