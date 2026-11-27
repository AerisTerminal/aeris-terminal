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
    NativeStudyProgram, StudyExecutionContext, StudyFillSpec, StudyInputSeries, fixed_point_to_f64,
};
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
/// Required guest linear memory export name.
pub const WASM_EXPORT_MEMORY: &str = "memory";

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
    /// Uses `aeris_study_calculate_multi` when exported; otherwise falls back to
    /// `aeris_study_calculate` for single-output modules.
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
        let count = output_count.clamp(1, 16);
        if !bars.is_consistent() {
            return Err(WasmSandboxError::InconsistentInputBars);
        }
        let bar_count = bars.len();
        if bar_count == 0 {
            return Ok(WasmMultiExecutionResult {
                outputs: vec![Vec::new(); count],
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

        let remaining_fuel = store.get_fuel().unwrap_or(0);
        let fuel_consumed = initial_fuel.saturating_sub(remaining_fuel);
        Ok(WasmMultiExecutionResult {
            outputs,
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
        self.into_native_program_with_spec(1, Vec::new(), limits)
    }

    /// Wraps a multi-output sandboxed WebAssembly module (with optional fills) into a
    /// [`NativeStudyProgram`] that runs inside `StudyRuntime`.
    #[must_use]
    pub fn into_native_program_with_spec(
        self,
        output_count: usize,
        fills: Vec<StudyFillSpec>,
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

            let result = compiled
                .execute_columnar_outputs(&columnar, count, limits)
                .map_err(|err| err.to_string())?;

            for (out_idx, series) in result.outputs.into_iter().enumerate() {
                let Some(output) = context.output(out_idx) else {
                    break;
                };
                for (offset, value) in series.into_iter().enumerate() {
                    output.set(start + offset, value)?;
                }
            }
            for &fill in &fills {
                context.add_fill(fill)?;
            }
            Ok(())
        }))
    }
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
