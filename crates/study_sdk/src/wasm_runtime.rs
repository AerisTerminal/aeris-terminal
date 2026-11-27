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
    MAXIMUM_STUDY_POLYLINE_POINTS, MAXIMUM_STUDY_SCENE_OBJECTS, MAXIMUM_STUDY_SCENE_TEXT_BYTES,
    MAXIMUM_STUDY_TABLE_CELLS, NativeStudyProgram, StudyBoxObject, StudyColor,
    StudyExecutionContext, StudyFillSpec, StudyInputSeries, StudyLabelObject, StudyLabelStyle,
    StudyLevelSpec, StudyLineExtend, StudyLineObject, StudyLineStyle, StudyPoint,
    StudyPolylineObject, StudySceneObject, StudySceneState, StudySettingValue, StudyTableCell,
    StudyTableObject, StudyTablePosition, StudyTextAlign, fixed_point_to_f64,
};
use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::sync::Arc;
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
/// Optional guest export setting the chart's visible bar index range `(left: u32, right: u32)`.
pub const WASM_EXPORT_SET_VIEWPORT: &str = "aeris_study_set_viewport";
/// Optional guest export returning the linear-memory pointer of the encoded scene command buffer.
pub const WASM_EXPORT_SCENE_PTR: &str = "aeris_study_scene_ptr";
/// Optional guest export returning the byte length of the encoded scene command buffer.
pub const WASM_EXPORT_SCENE_LEN: &str = "aeris_study_scene_len";
/// Required guest linear memory export name.
pub const WASM_EXPORT_MEMORY: &str = "memory";

/// Maximum encoded scene command buffer size read from one sandbox execution (256 KiB).
const MAX_WASM_SCENE_BYTES: usize = 256 * 1024;
const OP_BOX: u8 = 1;
const OP_LINE: u8 = 2;
const OP_POLYLINE: u8 = 3;
const OP_LABEL: u8 = 4;
const OP_TABLE_CELL: u8 = 5;
const OP_BAR_COLOR: u8 = 6;
const OP_BG_COLOR: u8 = 7;

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

/// Execution telemetry returned alongside multi-output computed study series.
#[derive(Clone, Debug, PartialEq)]
pub struct WasmMultiExecutionResult {
    /// One output vector per declared output series, each aligned 1:1 with the input bars.
    pub outputs: Vec<Vec<Option<f64>>>,
    /// Optional binary scene command buffer emitted by the guest study (boxes, lines, polylines,
    /// labels, tables, per-bar colors, background colors).
    pub scene_bytes: Vec<u8>,
    /// Exact instruction fuel consumed by this execution wave.
    pub fuel_consumed: u64,
}

struct SandboxStoreState {
    limits: StoreLimits,
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
        let count = output_count.clamp(1, 16);
        if !bars.is_consistent() {
            return Err(WasmSandboxError::InconsistentInputBars);
        }
        let bar_count = bars.len();
        if bar_count == 0 {
            return Ok(WasmMultiExecutionResult {
                outputs: vec![Vec::new(); count],
                scene_bytes: Vec::new(),
                fuel_consumed: 0,
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
        apply_wasm_viewport(instance, &mut store, viewport, bar_count)?;

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

        let remaining_fuel = store.get_fuel().unwrap_or(0);
        let fuel_consumed = initial_fuel.saturating_sub(remaining_fuel);
        Ok(WasmMultiExecutionResult {
            outputs,
            scene_bytes,
            fuel_consumed,
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
        let compiled = self;
        let count = output_count.clamp(1, 16);
        NativeStudyProgram::dynamic(Arc::new(move |context: &mut StudyExecutionContext<'_>| {
            let Some(StudyInputSeries::Market(market)) = context.input(0) else {
                return Err("sandboxed study requires primary market bar input".to_string());
            };
            let bars = market.bars();
            let bar_count = bars.len();
            if bar_count == 0 {
                return Ok(());
            }
            let dirty = context.dirty_range();
            let dirty_start = dirty.start.min(bar_count);
            let dirty_end = dirty.end_exclusive.unwrap_or(bar_count).min(bar_count);
            let params: Vec<f64> = param_identifiers
                .iter()
                .map(|id| match context.settings().get(id) {
                    Some(StudySettingValue::Integer(v)) => {
                        fixed_point_to_f64(*v, 0).unwrap_or(f64::NAN)
                    }
                    Some(StudySettingValue::Decimal(d) | StudySettingValue::Price(d)) => {
                        fixed_point_to_f64(d.mantissa, d.scale).unwrap_or(f64::NAN)
                    }
                    Some(StudySettingValue::Boolean(b)) => {
                        if *b {
                            1.0
                        } else {
                            0.0
                        }
                    }
                    _ => f64::NAN,
                })
                .collect();

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
            let result = compiled
                .execute_columnar_full(&columnar, count, &params, viewport, limits)
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
                    &columnar.timestamps_unix_nanos,
                    start,
                    write_from,
                    write_to,
                )?;
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
    for (idx, &val) in params.iter().take(16).enumerate() {
        if let Ok(idx_u32) = u32::try_from(idx) {
            set_param_fn
                .call(&mut *store, (idx_u32, val))
                .map_err(|err| classify_wasmi_error(&err, false))?;
        }
    }
    Ok(())
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
    bar_colors: Vec<(usize, StudyColor)>,
    bg_colors: Vec<(usize, StudyColor)>,
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

fn decode_wasm_scene_commands(bytes: &[u8], timestamps: &[i64]) -> DecodedWasmScene {
    let step_ns = estimate_bar_step_ns(timestamps);
    let mut cursor = SceneCursor { bytes, pos: 0 };
    let mut objects: Vec<StudySceneObject> = Vec::new();
    let mut tables: BTreeMap<(u8, u16, u16), StudyTableObject> = BTreeMap::new();
    let mut bar_colors: Vec<(usize, StudyColor)> = Vec::new();
    let mut bg_colors: Vec<(usize, StudyColor)> = Vec::new();

    while let Some(op) = cursor.u8() {
        let ok = match op {
            OP_BOX => decode_box_cmd(&mut cursor, timestamps, step_ns, &mut objects),
            OP_LINE => decode_line_cmd(&mut cursor, timestamps, step_ns, &mut objects),
            OP_POLYLINE => decode_polyline_cmd(&mut cursor, timestamps, step_ns, &mut objects),
            OP_LABEL => decode_label_cmd(&mut cursor, timestamps, step_ns, &mut objects),
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
            _ => false,
        };
        if !ok {
            break;
        }
    }

    for table in tables.into_values() {
        if objects.len() < MAXIMUM_STUDY_SCENE_OBJECTS {
            objects.push(StudySceneObject::Table(table));
        }
    }
    DecodedWasmScene {
        objects,
        bar_colors,
        bg_colors,
    }
}

fn reconcile_wasm_scene(
    scene: &mut StudySceneState,
    objects: Vec<StudySceneObject>,
) -> Result<(), String> {
    let existing_ids: Vec<_> = scene.iter().map(|(id, _)| id).collect();
    let keep = existing_ids.len().min(objects.len());
    let mut iter = objects.into_iter();
    for &id in &existing_ids[..keep] {
        if let Some(next_obj) = iter.next()
            && scene.get(id) != Some(&next_obj)
        {
            scene.update(id, next_obj)?;
        }
    }
    for extra in iter {
        let _ = scene.insert(extra)?;
    }
    for &stale_id in &existing_ids[keep..] {
        let _ = scene.delete(stale_id);
    }
    Ok(())
}

fn apply_decoded_wasm_scene(
    context: &mut StudyExecutionContext<'_>,
    scene_bytes: &[u8],
    timestamps: &[i64],
    slice_start: usize,
    write_from: usize,
    write_to: usize,
) -> Result<(), String> {
    let decoded = decode_wasm_scene_commands(scene_bytes, timestamps);
    reconcile_wasm_scene(context.scene_mut(), decoded.objects)?;
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
}
