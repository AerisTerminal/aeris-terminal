#![cfg(test)]

use super::*;
use aeris_market_data::{BarPeriod, DepthSnapshot, EventMetadata, QualifiedTimestamp};
use aeris_market_engine::{
    MarketEngineConfig, MarketStream, ProviderCapabilities, ProviderConfig, ProviderGeneration,
};
use std::{sync::Mutex, time::Duration};

fn bound(value: usize) -> NonZeroUsize {
    NonZeroUsize::new(value).expect("test bounds are non-zero")
}

fn config(maximum_studies: usize) -> StudyRuntimeConfig {
    StudyRuntimeConfig {
        maximum_studies: bound(maximum_studies),
        maximum_dependencies_per_study: bound(8),
        maximum_outputs_per_study: bound(8),
        maximum_points_per_output: bound(64),
        maximum_total_output_points: bound(1_024),
        maximum_state_bytes_per_study: bound(1_024),
        maximum_total_state_bytes: bound(8_192),
    }
}

fn series(instrument: &str) -> BarSeriesKey {
    series_period(instrument, 60)
}

fn series_period(instrument: &str, seconds: u32) -> BarSeriesKey {
    BarSeriesKey {
        provider_id: "provider".to_string(),
        instrument_id: instrument.to_string(),
        entitlement_id: "entitlement".to_string(),
        period: BarPeriod::Time { seconds },
        definition_version: 1,
    }
}

fn engine(maximum_series: usize) -> MarketEngine {
    let mut engine = MarketEngine::new(MarketEngineConfig {
        maximum_consumers: bound(4),
        maximum_series: bound(maximum_series),
        maximum_bars: bound(maximum_series.saturating_mul(64).max(1)),
    });
    engine
        .register_provider(
            "provider".to_string(),
            ProviderConfig {
                account_id: "provider:test".to_string(),
                capabilities: ProviderCapabilities {
                    historical_bars: true,
                    realtime_bars: true,
                    streams: StreamRequirements::BARS
                        .with(MarketStream::Trades)
                        .with(MarketStream::Quotes)
                        .with(MarketStream::Depth),
                },
                reconnect_delay: Duration::from_millis(250),
            },
        )
        .expect("test provider registers");
    engine
        .begin_provider_session("provider", ProviderGeneration(NonZeroU64::MIN))
        .expect("test provider session begins");
    engine
}

fn bars() -> Vec<MarketBar> {
    vec![
        MarketBar {
            source_sequence: 1,
            exchange_timestamp_seconds: 1_700_000_000,
            exchange_timestamp_unix_nanos: 1_700_000_000_000_000_000,
            open: 10_000,
            high: 11_000,
            low: 9_000,
            close: 10_500,
            volume: 1_250,
        },
        MarketBar {
            source_sequence: 2,
            exchange_timestamp_seconds: 1_700_000_060,
            exchange_timestamp_unix_nanos: 1_700_000_060_000_000_000,
            open: 10_500,
            high: 11_500,
            low: 10_000,
            close: 11_000,
            volume: 1_500,
        },
    ]
}

fn bar(source_sequence: u64, offset_seconds: i64, close: i64) -> MarketBar {
    let exchange_timestamp_seconds = 1_700_000_000 + offset_seconds;
    MarketBar {
        source_sequence,
        exchange_timestamp_seconds,
        exchange_timestamp_unix_nanos: exchange_timestamp_seconds * 1_000_000_000,
        open: close,
        high: close + 100,
        low: close - 100,
        close,
        volume: 1_000,
    }
}

fn event_metadata(source_sequence: u64, offset_seconds: i64) -> EventMetadata {
    let observed_unix_nanos = (1_700_000_000 + offset_seconds) * 1_000_000_000;
    EventMetadata {
        provider_id: "provider".to_string(),
        instrument_id: "ES".to_string(),
        entitlement_id: "entitlement".to_string(),
        source_sequence,
        session_generation: 1,
        timestamps: QualifiedTimestamp {
            exchange_unix_nanos: Some(observed_unix_nanos),
            provider_unix_nanos: None,
            received_unix_nanos: observed_unix_nanos + 1,
        },
    }
}

fn market(series: BarSeriesKey, streams: StreamRequirements) -> StudyDependency {
    StudyDependency::Market(StudyMarketInput { series, streams })
}

fn outputs(count: usize) -> Vec<StudyOutputSpec> {
    (0..count)
        .map(|index| StudyOutputSpec {
            identifier: format!("output_{index}"),
            title: format!("Output {}", index + 1),
            legend_label: None,
            plot: StudyPlotKind::Line,
            pane: StudyPaneTarget::Price,
            scale: StudyScaleTarget::Primary,
            threshold_region: None,
            point_style: StudyPointStyle::Uniform,
        })
        .collect()
}

fn definition(
    identifier: &str,
    dependencies: Vec<StudyDependency>,
    output_count: usize,
    invalidation: StudyInvalidationPolicy,
) -> StudyDefinition {
    StudyDefinition {
        identifier: identifier.to_string(),
        dependencies,
        settings: Vec::new(),
        outputs: outputs(output_count),
        invalidation,
    }
}

fn calculate_scaled_close(context: &mut StudyExecutionContext<'_>) -> Result<(), String> {
    let (inputs, outputs) = context.split();
    let multiplier = match inputs.settings().get("multiplier") {
        Some(StudySettingValue::Integer(value)) => *value,
        _ => return Err("multiplier setting is unavailable".to_string()),
    };
    let close = match inputs.input(0) {
        Some(StudyInputSeries::Market(series)) => series.field(StudyBarField::Close),
        _ => return Err("primary market input is unavailable".to_string()),
    };
    let output = outputs
        .get_mut(0)
        .ok_or_else(|| "primary output is unavailable".to_string())?;
    let multiplier = f64::from(
        i32::try_from(multiplier).map_err(|_| "test multiplier is out of range".to_string())?,
    );
    let dirty = inputs.dirty_range();
    let end = dirty
        .end_exclusive
        .unwrap_or(output.len())
        .min(output.len());
    for index in dirty.start..end {
        let value = close
            .value(index)
            .ok_or_else(|| "close row is unavailable".to_string())?;
        let value =
            f64::from(i32::try_from(value).map_err(|_| "test close is out of range".to_string())?);
        output.set(index, Some(value * multiplier))?;
    }
    Ok(())
}

fn calculate_double_output(context: &mut StudyExecutionContext<'_>) -> Result<(), String> {
    let (inputs, outputs) = context.split();
    let Some(StudyInputSeries::Output(source)) = inputs.input(0) else {
        return Err("upstream study output is unavailable".to_string());
    };
    let output = outputs
        .get_mut(0)
        .ok_or_else(|| "downstream output is unavailable".to_string())?;
    let dirty = inputs.dirty_range();
    let end = dirty
        .end_exclusive
        .unwrap_or(output.len())
        .min(output.len());
    for index in dirty.start..end {
        output.set(
            index,
            source.value(index).flatten().map(|value| value * 2.0),
        )?;
    }
    Ok(())
}

fn calculate_then_fail(context: &mut StudyExecutionContext<'_>) -> Result<(), String> {
    let (_, outputs) = context.split();
    let output = outputs
        .get_mut(0)
        .ok_or_else(|| "failure-test output is unavailable".to_string())?;
    if !output.is_empty() {
        output.set(0, Some(999.0))?;
    }
    Err("intentional calculation rejection".to_string())
}

fn calculate_scaled_close_then_fail_incremental(
    context: &mut StudyExecutionContext<'_>,
) -> Result<(), String> {
    if context.dirty_range().start == 0 {
        return calculate_scaled_close(context);
    }
    let dirty = context.dirty_range();
    if let Some(output) = context.output(0)
        && dirty.start < output.len()
    {
        output.set(dirty.start, Some(999.0))?;
    }
    Err("intentional incremental calculation rejection".to_string())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct TestNativeState {
    executions: u32,
    accounted_bytes: usize,
}

fn test_native_state_bytes(state: &TestNativeState) -> usize {
    state.accounted_bytes
}

fn panicking_test_native_state_bytes(_state: &TestNativeState) -> usize {
    panic!("intentional state accounting panic");
}

struct SharedTransactionalState {
    executions: Arc<Mutex<u32>>,
}

fn shared_transactional_state_bytes(_state: &SharedTransactionalState) -> usize {
    std::mem::size_of::<SharedTransactionalState>() + std::mem::size_of::<u32>()
}

fn clone_shared_transactional_state(state: &SharedTransactionalState) -> SharedTransactionalState {
    let executions = *state
        .executions
        .lock()
        .expect("shared transactional state lock");
    SharedTransactionalState {
        executions: Arc::new(Mutex::new(executions)),
    }
}

fn create_shared_transactional_state(settings: &StudySettings) -> Result<NativeStudyState, String> {
    if !settings.is_empty() {
        return Err("shared transactional state declares no settings".to_string());
    }
    Ok(NativeStudyState::new_transactional(
        SharedTransactionalState {
            executions: Arc::new(Mutex::new(0)),
        },
        shared_transactional_state_bytes,
        clone_shared_transactional_state,
    ))
}

fn calculate_shared_transactional_state(
    context: &mut StudyExecutionContext<'_>,
) -> Result<(), String> {
    let dirty = context.dirty_range();
    let state = context
        .state_mut::<SharedTransactionalState>()
        .ok_or_else(|| "shared transactional state is unavailable".to_string())?;
    let mut executions = state
        .executions
        .lock()
        .map_err(|_| "shared transactional state lock is poisoned".to_string())?;
    *executions = executions.saturating_add(if dirty.start == 0 { 1 } else { 100 });
    let value = f64::from(*executions);
    drop(executions);
    if let Some(output) = context.output(0)
        && dirty.start < output.len()
    {
        output.set(dirty.start, Some(value))?;
    }
    if dirty.start > 0 {
        return Err("intentional shared-state candidate rejection".to_string());
    }
    Ok(())
}

fn create_test_native_state(settings: &StudySettings) -> Result<NativeStudyState, String> {
    let accounted_bytes = match settings.get("state_bytes") {
        Some(StudySettingValue::Integer(value)) => usize::try_from(*value)
            .map_err(|_| "test state byte setting is out of range".to_string())?,
        _ => return Err("test state byte setting is unavailable".to_string()),
    };
    Ok(NativeStudyState::new(
        TestNativeState {
            executions: 0,
            accounted_bytes,
        },
        test_native_state_bytes,
    ))
}

fn create_panicking_accounted_state(settings: &StudySettings) -> Result<NativeStudyState, String> {
    if settings.get("state_bytes").is_none() {
        return Err("test state byte setting is unavailable".to_string());
    }
    Ok(NativeStudyState::new(
        TestNativeState {
            executions: 0,
            accounted_bytes: 8,
        },
        panicking_test_native_state_bytes,
    ))
}

fn calculate_stateful_counter(context: &mut StudyExecutionContext<'_>) -> Result<(), String> {
    let (inputs, state, outputs) = context
        .split_with_state::<TestNativeState>()
        .ok_or_else(|| "test native state is unavailable".to_string())?;
    state.executions = state.executions.saturating_add(1);
    let output = outputs
        .get_mut(0)
        .ok_or_else(|| "stateful test output is unavailable".to_string())?;
    let dirty = inputs.dirty_range();
    let end = dirty
        .end_exclusive
        .unwrap_or(output.len())
        .min(output.len());
    for index in dirty.start..end {
        output.set(index, Some(f64::from(state.executions)))?;
    }
    Ok(())
}

fn calculate_stateful_then_fail(context: &mut StudyExecutionContext<'_>) -> Result<(), String> {
    let (_, state, outputs) = context
        .split_with_state::<TestNativeState>()
        .ok_or_else(|| "test native state is unavailable".to_string())?;
    state.executions = state.executions.saturating_add(100);
    if let Some(output) = outputs.first_mut()
        && !output.is_empty()
    {
        output.set(0, Some(999.0))?;
    }
    Err("intentional stateful rejection".to_string())
}

fn calculate_stateful_then_panic(context: &mut StudyExecutionContext<'_>) -> Result<(), String> {
    let state = context
        .state_mut::<TestNativeState>()
        .ok_or_else(|| "test native state is unavailable".to_string())?;
    state.executions = state.executions.saturating_add(100);
    panic!("intentional stateful panic");
}

fn calculate_stateful_growth(context: &mut StudyExecutionContext<'_>) -> Result<(), String> {
    let (_, state, outputs) = context
        .split_with_state::<TestNativeState>()
        .ok_or_else(|| "test native state is unavailable".to_string())?;
    state.executions = state.executions.saturating_add(1);
    state.accounted_bytes = 2_048;
    if let Some(output) = outputs.first_mut()
        && !output.is_empty()
    {
        output.set(0, Some(999.0))?;
    }
    Ok(())
}

fn stateful_registration(source: BarSeriesKey, accounted_bytes: usize) -> NativeStudyRegistration {
    stateful_registration_with_dependency(market(source, StreamRequirements::BARS), accounted_bytes)
}

fn stateful_registration_with_dependency(
    dependency: StudyDependency,
    accounted_bytes: usize,
) -> NativeStudyRegistration {
    let definition = StudyDefinition {
        identifier: "stateful_counter".to_string(),
        dependencies: vec![dependency],
        settings: vec![StudySettingSpec::new(
            "state_bytes",
            StudySettingValue::Integer(1),
        )],
        outputs: outputs(1),
        invalidation: StudyInvalidationPolicy::SameRange,
    };
    let settings = StudySettings::with_overrides(
        &definition.settings,
        BTreeMap::from([(
            "state_bytes".to_string(),
            StudySettingValue::Integer(
                i64::try_from(accounted_bytes).expect("test state size fits i64"),
            ),
        )]),
    )
    .expect("stateful settings are valid");
    NativeStudyRegistration {
        settings,
        definition,
        program: NativeStudyProgram {
            calculate: calculate_stateful_counter,
            state_factory: Some(create_test_native_state),
        },
    }
}

fn committed_test_state(runtime: &mut StudyRuntime, study_id: StudyInstanceId) -> TestNativeState {
    runtime
        .studies
        .get_mut(&study_id)
        .and_then(|node| node.state.as_mut())
        .and_then(NativeStudyState::value_mut::<TestNativeState>)
        .copied()
        .expect("test native state is committed")
}

fn committed_shared_state_executions(runtime: &StudyRuntime, study_id: StudyInstanceId) -> u32 {
    *runtime
        .studies
        .get(&study_id)
        .and_then(|node| node.state.as_ref())
        .and_then(NativeStudyState::value::<SharedTransactionalState>)
        .expect("committed shared transactional state")
        .executions
        .lock()
        .expect("committed shared transactional state lock")
}

#[test]
#[ignore = "explicit release soak qualification for sustained trusted-native tail work"]
fn concurrent_native_study_tail_soak_keeps_state_output_and_demand_bounded() {
    const STUDIES: usize = 16;
    const TAIL_ITERATIONS: u32 = 20_000;

    let mut runtime = StudyRuntime::new(config(32));
    let mut engine = engine(2);
    let source = series("ES");
    engine
        .install_history(ProviderGeneration(NonZeroU64::MIN), &source, 2, 3, bars())
        .expect("canonical history installs");

    let studies = (0..STUDIES)
        .map(|_| {
            runtime
                .register_native_for_consumer(
                    ConsumerId(NonZeroU64::MIN),
                    stateful_registration(source.clone(), 8),
                )
                .expect("trusted native study registers")
        })
        .collect::<Vec<_>>();
    assert_eq!(runtime.market_requirements().len(), 1);
    let lease_changes = runtime
        .reconcile_market_leases(&mut engine)
        .expect("shared study market lease reconciles");
    assert_eq!(lease_changes.len(), 1);
    assert_eq!(engine.data_lease_count(), 1);
    assert_eq!(
        engine
            .subscription_status(&source)
            .map(|status| (status.consumer_count, status.streams)),
        Some((0, StreamRequirements::BARS))
    );

    for study in &studies {
        runtime
            .execute_ready(&engine, *study)
            .expect("initial covering execution succeeds");
    }
    let expected_points = STUDIES * bars().len();
    let expected_state_bytes = STUDIES * 8;
    assert_eq!(runtime.output_points, expected_points);
    assert_eq!(runtime.state_bytes, expected_state_bytes);

    let tail = StudyDirtyRange::bounded(1, 2).expect("one-row tail range");
    for _ in 0..TAIL_ITERATIONS {
        for study in &studies {
            runtime
                .execute_ready_range(&engine, *study, Some(tail))
                .expect("bounded tail execution succeeds");
        }
    }

    assert_eq!(runtime.output_points, expected_points);
    assert_eq!(runtime.state_bytes, expected_state_bytes);
    assert_eq!(runtime.market_requirements().len(), 1);
    assert_eq!(engine.data_lease_count(), 1);
    assert_eq!(
        engine
            .subscription_status(&source)
            .map(|status| (status.consumer_count, status.streams)),
        Some((0, StreamRequirements::BARS))
    );
    for study in studies {
        let state = committed_test_state(&mut runtime, study);
        assert_eq!(state.executions, TAIL_ITERATIONS + 1);
        assert_eq!(state.accounted_bytes, 8);
        let output = runtime
            .output_series(study.output(0))
            .expect("study output remains installed");
        assert_eq!(output.len(), 2);
    }
}

#[test]
#[ignore = "explicit release soak qualification for repeated reinitialize and historical repair"]
fn native_study_reinitialize_and_historical_repair_soak_keeps_accounting_bounded() {
    const CYCLES: u32 = 2_000;
    const BAR_COUNT: usize = 64;

    let mut runtime = StudyRuntime::new(config(4));
    let mut engine = engine(1);
    let source = series("ES");
    let history = (0..BAR_COUNT)
        .map(|index| {
            bar(
                u64::try_from(index + 1).expect("small soak sequence"),
                i64::try_from(index * 60).expect("small soak timestamp"),
                10_000 + i64::try_from(index).expect("small soak close"),
            )
        })
        .collect::<Vec<_>>();
    engine
        .install_history(ProviderGeneration(NonZeroU64::MIN), &source, 2, 3, history)
        .expect("canonical soak history installs");
    let owner = ConsumerId(NonZeroU64::MIN);
    let study = runtime
        .register_native_for_consumer(owner, stateful_registration(source.clone(), 8))
        .expect("stateful soak study registers");
    runtime
        .execute_ready(&engine, study)
        .expect("initial covering execution succeeds");

    let repair = StudyDirtyRange::bounded(8, 24).expect("bounded historical repair");
    for _ in 0..CYCLES {
        assert_eq!(
            runtime
                .reinitialize_native_for_consumer(
                    owner,
                    study,
                    stateful_registration(source.clone(), 8),
                )
                .expect("study reinitializes in place"),
            vec![study]
        );
        runtime
            .execute_ready(&engine, study)
            .expect("covering replay after reinitialize succeeds");
        runtime
            .execute_ready_range(&engine, study, Some(repair))
            .expect("historical repair succeeds");

        assert_eq!(runtime.output_points, BAR_COUNT);
        assert_eq!(runtime.state_bytes, 8);
        assert_eq!(runtime.market_requirements().len(), 1);
        assert_eq!(
            runtime
                .output_series(study.output(0))
                .expect("study output remains installed")
                .len(),
            BAR_COUNT
        );
    }
    let state = committed_test_state(&mut runtime, study);
    assert_eq!(state.executions, 2);
    assert_eq!(state.accounted_bytes, 8);
}

#[test]
#[ignore = "explicit release soak qualification for bar-aligned non-bar burst work"]
fn native_non_bar_burst_soak_keeps_output_and_demand_bounded() {
    const EVENTS: u32 = 50_000;

    let mut runtime = StudyRuntime::new(config(4));
    let mut engine = engine(1);
    let source = series("ES");
    engine
        .install_history(ProviderGeneration(NonZeroU64::MIN), &source, 2, 0, bars())
        .expect("canonical history installs");
    let streams = StreamRequirements::BARS
        .with(MarketStream::Trades)
        .with(MarketStream::Quotes)
        .with(MarketStream::Depth);
    let definition = definition(
        "live_microstructure_soak",
        vec![market(source, streams)],
        1,
        StudyInvalidationPolicy::FromFirstChanged,
    );
    let study = runtime
        .register_native_for_consumer(
            ConsumerId(NonZeroU64::MIN),
            NativeStudyRegistration {
                settings: StudySettings::defaults(&definition.settings).expect("defaults"),
                definition,
                program: NativeStudyProgram::stateless(calculate_live_microstructure),
            },
        )
        .expect("microstructure soak study registers");
    let live = live_microstructure_fixture();
    let first_live = StudyLiveMarketData::new(
        Some(StudyQuoteView::new(&live.quote_one, 2, 0)),
        Some(StudyTradeWindow::new(&live.trades, 1, 2, 2, 0)),
        Some(StudyDepthView::new(&live.book, 2, 0)),
    );
    let mut initial_lookup = |_input: &StudyMarketInput| Some(first_live);
    runtime
        .execute_ready_with_live(&engine, study, &mut initial_lookup)
        .expect("initial microstructure execution succeeds");

    let second_live = StudyLiveMarketData::new(
        Some(StudyQuoteView::new(&live.quote_two, 2, 0)),
        Some(StudyTradeWindow::new(&live.trades, 1, 2, 2, 0)),
        Some(StudyDepthView::new(&live.book, 2, 0)),
    );
    for index in 0..EVENTS {
        let current_live = if index % 2 == 0 {
            second_live
        } else {
            first_live
        };
        let mut lookup = |_input: &StudyMarketInput| Some(current_live);
        let stream = match index % 3 {
            0 => MarketStream::Quotes,
            1 => MarketStream::Trades,
            _ => MarketStream::Depth,
        };
        let batch = runtime
            .execute_live_non_bar_change_with_live(
                &engine,
                StudyNonBarChange {
                    provider_id: "provider",
                    instrument_id: "ES",
                    entitlement_id: "entitlement",
                    stream,
                    observed_unix_nanos: event_metadata(u64::from(index) + 10, 90)
                        .timestamps
                        .exchange_unix_nanos
                        .expect("exchange timestamp"),
                },
                &mut lookup,
                &mut |_| true,
            )
            .expect("non-bar burst event recalculates");
        assert_eq!(batch.executed, vec![study]);
        assert_eq!(batch.errors, [] as [StudyRuntimeError; 0]);
    }

    assert_eq!(runtime.output_points, bars().len());
    assert_eq!(runtime.state_bytes, 0);
    assert_eq!(runtime.market_requirements().len(), 1);
    let output = runtime
        .output_series(study.output(0))
        .expect("microstructure output remains installed");
    assert_eq!(output.len(), bars().len());
    assert!(output.values().iter().all(Option::is_some));
}

fn calculate_secondary_asof(context: &mut StudyExecutionContext<'_>) -> Result<(), String> {
    let (inputs, outputs) = context.split();
    let Some(StudyInputSeries::Market(_primary)) = inputs.input(0) else {
        return Err("primary market input is unavailable".to_string());
    };
    let Some(StudyInputSeries::Market(secondary)) = inputs.input(1) else {
        return Err("secondary market input is unavailable".to_string());
    };
    let output = outputs
        .get_mut(0)
        .ok_or_else(|| "MTF output is unavailable".to_string())?;
    let dirty = inputs.dirty_range();
    let end = dirty
        .end_exclusive
        .unwrap_or(output.len())
        .min(output.len());
    for index in dirty.start..end {
        let timestamp = *inputs
            .timestamps()
            .get(index)
            .ok_or_else(|| "primary timestamp is unavailable".to_string())?;
        let insertion = secondary
            .bars()
            .partition_point(|bar| bar.exchange_timestamp_unix_nanos <= timestamp);
        let value = insertion
            .checked_sub(1)
            .and_then(|secondary_index| secondary.bars().get(secondary_index))
            .map(|bar| bar.close)
            .map(i32::try_from)
            .transpose()
            .map_err(|_| "secondary close is out of test range".to_string())?
            .map(f64::from);
        output.set(index, value)?;
    }
    Ok(())
}

fn calculate_live_microstructure(context: &mut StudyExecutionContext<'_>) -> Result<(), String> {
    let (inputs, outputs) = context.split();
    let live = inputs
        .live_market(0)
        .ok_or_else(|| "live market state is unavailable".to_string())?;
    let quote = live
        .quote()
        .ok_or_else(|| "quote is unavailable".to_string())?;
    let trades = live
        .trades()
        .ok_or_else(|| "trades are unavailable".to_string())?;
    let depth = live
        .depth()
        .ok_or_else(|| "depth is unavailable".to_string())?;
    let bid = quote
        .bid()
        .ok_or_else(|| "quote bid is unavailable".to_string())?
        .price;
    let ask = quote
        .ask()
        .ok_or_else(|| "quote ask is unavailable".to_string())?
        .price;
    let trade_quantity = trades
        .iter()
        .last()
        .ok_or_else(|| "retained trade is unavailable".to_string())?
        .quantity;
    let depth_bid = depth
        .bids()
        .next()
        .ok_or_else(|| "depth bid is unavailable".to_string())?
        .price;
    let value = i32::try_from((ask - bid) + trade_quantity + depth_bid)
        .map(f64::from)
        .map_err(|_| "microstructure test value is out of range".to_string())?;
    let output = outputs
        .first_mut()
        .ok_or_else(|| "microstructure output is unavailable".to_string())?;
    let dirty = inputs.dirty_range();
    let end = dirty
        .end_exclusive
        .unwrap_or(output.len())
        .min(output.len());
    for row in dirty.start..end {
        output.set(row, Some(value))?;
    }
    Ok(())
}

struct LiveMicrostructureFixture {
    book: OrderBook,
    trades: VecDeque<crate::RetainedMarketTrade>,
    quote_one: TopOfBookQuote,
    quote_two: TopOfBookQuote,
}

fn live_microstructure_fixture() -> LiveMicrostructureFixture {
    let mut book = OrderBook::new(bound(8));
    book.install_snapshot(&DepthSnapshot {
        metadata: event_metadata(1, 90),
        bids: vec![DepthLevel {
            price: 95,
            quantity: 4,
            order_count: Some(1),
        }],
        asks: vec![DepthLevel {
            price: 140,
            quantity: 5,
            order_count: Some(1),
        }],
    })
    .expect("depth installs");
    let trade_metadata = event_metadata(2, 90);
    let trades = VecDeque::from([crate::RetainedMarketTrade {
        ingestion_ordinal: 1,
        observed_unix_nanos: trade_metadata.timestamps.received_unix_nanos,
        trade: std::sync::Arc::new(aeris_market_data::MarketTrade {
            metadata: trade_metadata,
            trade_id: "fixture-trade".to_string(),
            price: 105,
            quantity: 3,
            aggressor: AggressorSide::Buy,
        }),
    }]);
    let quote_one = TopOfBookQuote {
        metadata: event_metadata(3, 90),
        bid: Some(DepthLevel {
            price: 100,
            quantity: 2,
            order_count: None,
        }),
        ask: Some(DepthLevel {
            price: 110,
            quantity: 2,
            order_count: None,
        }),
    };
    let quote_two = TopOfBookQuote {
        metadata: event_metadata(4, 90),
        ask: Some(DepthLevel {
            price: 130,
            quantity: 2,
            order_count: None,
        }),
        ..quote_one.clone()
    };
    LiveMicrostructureFixture {
        book,
        trades,
        quote_one,
        quote_two,
    }
}

fn scaled_close_registration(source: BarSeriesKey, multiplier: i64) -> NativeStudyRegistration {
    let definition = StudyDefinition {
        identifier: "scaled_close".to_string(),
        dependencies: vec![market(source, StreamRequirements::BARS)],
        settings: vec![StudySettingSpec::new(
            "multiplier",
            StudySettingValue::Integer(1),
        )],
        outputs: outputs(1),
        invalidation: StudyInvalidationPolicy::SameRange,
    };
    let settings = StudySettings::with_overrides(
        &definition.settings,
        BTreeMap::from([(
            "multiplier".to_string(),
            StudySettingValue::Integer(multiplier),
        )]),
    )
    .expect("scaled close settings are valid");
    NativeStudyRegistration {
        definition,
        settings,
        program: NativeStudyProgram {
            calculate: calculate_scaled_close,
            state_factory: None,
        },
    }
}

struct NativeChainFixture {
    runtime: StudyRuntime,
    engine: MarketEngine,
    source: BarSeriesKey,
    provider_generation: ProviderGeneration,
    producer: StudyInstanceId,
    consumer: StudyInstanceId,
}

fn native_chain(multiplier: i64) -> NativeChainFixture {
    let mut runtime = StudyRuntime::new(config(8));
    let mut engine = engine(2);
    let source = series("ES");
    let provider_generation = ProviderGeneration(NonZeroU64::MIN);
    engine
        .install_history(provider_generation, &source, 2, 3, bars())
        .expect("canonical history installs");
    let owner = ConsumerId(NonZeroU64::MIN);
    let producer = runtime
        .register_native_for_consumer(owner, scaled_close_registration(source.clone(), multiplier))
        .expect("producer registers");
    let consumer_definition = StudyDefinition {
        identifier: "double_scaled_close".to_string(),
        dependencies: vec![StudyDependency::Output(StudyOutputId {
            study_id: producer,
            output_index: 0,
        })],
        settings: Vec::new(),
        outputs: outputs(1),
        invalidation: StudyInvalidationPolicy::SameRange,
    };
    let consumer = runtime
        .register_native_for_consumer(
            owner,
            NativeStudyRegistration {
                settings: StudySettings::defaults(&consumer_definition.settings)
                    .expect("consumer defaults"),
                definition: consumer_definition,
                program: NativeStudyProgram {
                    calculate: calculate_double_output,
                    state_factory: None,
                },
            },
        )
        .expect("consumer registers");
    runtime
        .execute_ready_for_market(&engine, &source)
        .expect("initial chain executes");
    NativeChainFixture {
        runtime,
        engine,
        source,
        provider_generation,
        producer,
        consumer,
    }
}

fn output_generation(runtime: &StudyRuntime, study_id: StudyInstanceId) -> u64 {
    runtime
        .output_series(study_id.output(0))
        .expect("study output is committed")
        .generation()
}

struct FailureIsolationFixture {
    runtime: StudyRuntime,
    engine: MarketEngine,
    source: BarSeriesKey,
    generation: ProviderGeneration,
    first_failing: StudyInstanceId,
    dependent: StudyInstanceId,
    second_failing: StudyInstanceId,
    independent: StudyInstanceId,
    generations: [u64; 4],
}

fn failure_isolation_fixture() -> FailureIsolationFixture {
    let mut runtime = StudyRuntime::new(config(8));
    let mut engine = engine(2);
    let source = series("ES");
    let generation = ProviderGeneration(NonZeroU64::MIN);
    engine
        .install_history(generation, &source, 2, 3, bars())
        .expect("canonical history installs");
    let owner = ConsumerId(NonZeroU64::MIN);

    let mut first_registration = scaled_close_registration(source.clone(), 2);
    first_registration.program.calculate = calculate_scaled_close_then_fail_incremental;
    let first_failing = runtime
        .register_native_for_consumer(owner, first_registration)
        .expect("first failing study registers");
    let dependent_definition = StudyDefinition {
        identifier: "blocked_dependent".to_string(),
        dependencies: vec![
            StudyDependency::Output(first_failing.output(0)),
            market(source.clone(), StreamRequirements::BARS),
        ],
        settings: Vec::new(),
        outputs: outputs(1),
        invalidation: StudyInvalidationPolicy::SameRange,
    };
    let dependent = runtime
        .register_native_for_consumer(
            owner,
            NativeStudyRegistration {
                settings: StudySettings::defaults(&dependent_definition.settings)
                    .expect("dependent settings"),
                definition: dependent_definition,
                program: NativeStudyProgram::stateless(calculate_double_output),
            },
        )
        .expect("dependent study registers");
    let mut second_registration = scaled_close_registration(source.clone(), 3);
    second_registration.program.calculate = calculate_scaled_close_then_fail_incremental;
    let second_failing = runtime
        .register_native_for_consumer(owner, second_registration)
        .expect("second failing study registers");
    let independent = runtime
        .register_native_for_consumer(owner, scaled_close_registration(source.clone(), 4))
        .expect("independent study registers");
    runtime
        .execute_ready_for_market(&engine, &source)
        .expect("covering execution succeeds");
    let generations = [
        output_generation(&runtime, first_failing),
        output_generation(&runtime, dependent),
        output_generation(&runtime, second_failing),
        output_generation(&runtime, independent),
    ];
    FailureIsolationFixture {
        runtime,
        engine,
        source,
        generation,
        first_failing,
        dependent,
        second_failing,
        independent,
        generations,
    }
}

const LARGE_HISTORY_ROWS: usize = 16_384;
const LARGE_OUTPUT_COUNT: usize = 4;

struct LargeHistoryFixture {
    runtime: StudyRuntime,
    engine: MarketEngine,
    source: BarSeriesKey,
    generation: ProviderGeneration,
    study: StudyInstanceId,
    last_history_bar: MarketBar,
}

fn large_history_fixture() -> LargeHistoryFixture {
    let mut runtime = StudyRuntime::new(StudyRuntimeConfig {
        maximum_studies: bound(2),
        maximum_dependencies_per_study: bound(2),
        maximum_outputs_per_study: bound(LARGE_OUTPUT_COUNT),
        maximum_points_per_output: bound(LARGE_HISTORY_ROWS + 1),
        maximum_total_output_points: bound((LARGE_HISTORY_ROWS + 1) * (LARGE_OUTPUT_COUNT + 1)),
        maximum_state_bytes_per_study: bound(1_024),
        maximum_total_state_bytes: bound(2_048),
    });
    let mut engine = engine((LARGE_HISTORY_ROWS + 64) / 64);
    let source = series("ES-LARGE");
    let generation = ProviderGeneration(NonZeroU64::MIN);
    let history = (0..LARGE_HISTORY_ROWS)
        .map(|index| {
            bar(
                u64::try_from(index + 1).expect("history sequence"),
                i64::try_from(index).expect("history offset") * 60,
                10_000 + i64::try_from(index).expect("history close"),
            )
        })
        .collect::<Vec<_>>();
    let last_history_bar = *history.last().expect("large history has a tail");
    engine
        .install_history(generation, &source, 2, 0, history)
        .expect("large canonical history installs");
    let mut registration = scaled_close_registration(source.clone(), 2);
    registration.definition.outputs = outputs(LARGE_OUTPUT_COUNT);
    let study = runtime
        .register_native_for_consumer(ConsumerId(NonZeroU64::MIN), registration)
        .expect("large-history study registers");
    runtime
        .execute_ready(&engine, study)
        .expect("large-history covering execution succeeds");
    LargeHistoryFixture {
        runtime,
        engine,
        source,
        generation,
        study,
        last_history_bar,
    }
}

struct MtfFixture {
    runtime: StudyRuntime,
    engine: MarketEngine,
    secondary: BarSeriesKey,
    provider_generation: ProviderGeneration,
    study: StudyInstanceId,
    output: StudyOutputId,
}

fn mtf_fixture() -> MtfFixture {
    let mut runtime = StudyRuntime::new(config(4));
    let mut engine = engine(2);
    let provider_generation = ProviderGeneration(NonZeroU64::MIN);
    let primary = series_period("ES-1m", 60);
    let secondary = series_period("ES-5m", 300);
    engine
        .install_history(
            provider_generation,
            &primary,
            2,
            3,
            (0..7)
                .map(|index| {
                    bar(
                        u64::try_from(index + 1).expect("small sequence"),
                        i64::from(index) * 60,
                        10_000 + i64::from(index) * 100,
                    )
                })
                .collect(),
        )
        .expect("primary history installs");
    engine
        .install_history(
            provider_generation,
            &secondary,
            2,
            3,
            vec![bar(1, 0, 20_000)],
        )
        .expect("secondary history installs");
    let definition = StudyDefinition {
        identifier: "mtf_asof".to_string(),
        dependencies: vec![
            market(primary, StreamRequirements::BARS),
            market(secondary.clone(), StreamRequirements::BARS),
        ],
        settings: Vec::new(),
        outputs: outputs(1),
        invalidation: StudyInvalidationPolicy::SameRange,
    };
    let study = runtime
        .register_native_for_consumer(
            ConsumerId(NonZeroU64::MIN),
            NativeStudyRegistration {
                settings: StudySettings::defaults(&definition.settings).expect("defaults"),
                definition,
                program: NativeStudyProgram {
                    calculate: calculate_secondary_asof,
                    state_factory: None,
                },
            },
        )
        .expect("MTF study registers");
    assert!(
        runtime
            .execute_ready(&engine, study)
            .expect("initial MTF run")
    );
    MtfFixture {
        runtime,
        engine,
        secondary,
        provider_generation,
        study,
        output: StudyOutputId {
            study_id: study,
            output_index: 0,
        },
    }
}

#[test]
fn typed_settings_apply_only_declared_same_type_overrides() {
    let specs = vec![
        StudySettingSpec::new("length", StudySettingValue::Integer(20)),
        StudySettingSpec::new(
            "threshold",
            StudySettingValue::Decimal(StudyDecimal {
                mantissa: 125,
                scale: 2,
            }),
        ),
        StudySettingSpec::new("enabled", StudySettingValue::Boolean(true)),
    ];
    let settings = StudySettings::with_overrides(
        &specs,
        BTreeMap::from([("length".to_string(), StudySettingValue::Integer(50))]),
    )
    .expect("typed override is valid");
    assert_eq!(
        settings.get("length"),
        Some(&StudySettingValue::Integer(50))
    );
    assert_eq!(
        settings.get("threshold"),
        Some(&StudySettingValue::Decimal(StudyDecimal {
            mantissa: 125,
            scale: 2,
        }))
    );
    assert_eq!(
        settings.get("enabled"),
        Some(&StudySettingValue::Boolean(true))
    );

    assert_eq!(
        StudySettings::with_overrides(
            &specs,
            BTreeMap::from([("length".to_string(), StudySettingValue::Boolean(false),)]),
        )
        .expect_err("type mismatch must fail"),
        StudyRuntimeError::SettingTypeMismatch
    );
    assert_eq!(
        StudySettings::with_overrides(
            &specs,
            BTreeMap::from([("unknown".to_string(), StudySettingValue::Integer(1))]),
        )
        .expect_err("unknown setting must fail"),
        StudyRuntimeError::UnknownSetting
    );
}

#[test]
fn setting_presentation_constraints_validate_defaults_overrides_and_conditions() {
    let constrained = StudySettingSpec::new("period", StudySettingValue::Integer(20))
        .with_presentation(StudySettingPresentation {
            label: "Period".to_string(),
            description: Some("Bars used by the calculation".to_string()),
            group: Some("Inputs".to_string()),
            control: StudySettingControl::Integer {
                minimum: Some(1),
                maximum: Some(100),
                step: Some(1),
            },
            visible_when: None,
            enabled_when: None,
        });
    let dependent = StudySettingSpec::new("enabled", StudySettingValue::Boolean(true))
        .with_presentation(StudySettingPresentation {
            label: "Enabled".to_string(),
            description: None,
            group: Some("Inputs".to_string()),
            control: StudySettingControl::Boolean,
            visible_when: Some(StudySettingCondition {
                setting_identifier: "period".to_string(),
                equals: StudySettingValue::Integer(20),
            }),
            enabled_when: None,
        });
    let specs = vec![constrained, dependent];
    StudySettings::defaults(&specs).expect("presentation metadata validates");
    assert_eq!(
        StudySettings::with_overrides(
            &specs,
            BTreeMap::from([("period".to_string(), StudySettingValue::Integer(0))]),
        )
        .expect_err("minimum is enforced"),
        StudyRuntimeError::InvalidSettingValue
    );

    let invalid_condition = vec![
        StudySettingSpec::new("enabled", StudySettingValue::Boolean(true)).with_presentation(
            StudySettingPresentation {
                label: "Enabled".to_string(),
                description: None,
                group: None,
                control: StudySettingControl::Boolean,
                visible_when: Some(StudySettingCondition {
                    setting_identifier: "missing".to_string(),
                    equals: StudySettingValue::Boolean(true),
                }),
                enabled_when: None,
            },
        ),
    ];
    assert_eq!(
        StudySettings::defaults(&invalid_condition)
            .expect_err("unknown condition setting must fail"),
        StudyRuntimeError::InvalidSettingPresentation
    );
}

#[test]
fn native_execution_composes_market_and_study_outputs_in_dependency_order() {
    let fixture = native_chain(3);
    let producer_output = fixture
        .runtime
        .output_series(StudyOutputId {
            study_id: fixture.producer,
            output_index: 0,
        })
        .expect("producer output");
    assert_eq!(producer_output.values(), &[Some(31_500.0), Some(33_000.0)]);
    assert_eq!(producer_output.generation(), 1);
    assert_eq!(
        producer_output.timestamps(),
        &[1_700_000_000_000_000_000, 1_700_000_060_000_000_000,]
    );
    let consumer_output = fixture
        .runtime
        .output_series(StudyOutputId {
            study_id: fixture.consumer,
            output_index: 0,
        })
        .expect("consumer output");
    assert_eq!(consumer_output.values(), &[Some(63_000.0), Some(66_000.0)]);
    assert_eq!(consumer_output.generation(), 2);
    assert_eq!(consumer_output.timestamps(), producer_output.timestamps());
}

#[test]
fn non_bar_live_change_recomputes_the_containing_tail_row_from_borrowed_market_state() {
    let mut runtime = StudyRuntime::new(config(4));
    let mut engine = engine(1);
    let source = series("ES");
    engine
        .install_history(ProviderGeneration(NonZeroU64::MIN), &source, 2, 0, bars())
        .expect("canonical history installs");
    let streams = StreamRequirements::BARS
        .with(MarketStream::Trades)
        .with(MarketStream::Quotes)
        .with(MarketStream::Depth);
    let definition = definition(
        "live_microstructure",
        vec![market(source.clone(), streams)],
        1,
        StudyInvalidationPolicy::FromFirstChanged,
    );
    let study = runtime
        .register_native_for_consumer(
            ConsumerId(NonZeroU64::MIN),
            NativeStudyRegistration {
                settings: StudySettings::defaults(&definition.settings).expect("defaults"),
                definition,
                program: NativeStudyProgram::stateless(calculate_live_microstructure),
            },
        )
        .expect("study registers");

    let live = live_microstructure_fixture();

    let first_live = StudyLiveMarketData::new(
        Some(StudyQuoteView::new(&live.quote_one, 2, 0)),
        Some(StudyTradeWindow::new(&live.trades, 1, 2, 2, 0)),
        Some(StudyDepthView::new(&live.book, 2, 0)),
    );
    let mut first_lookup = |_input: &StudyMarketInput| Some(first_live);
    assert!(
        runtime
            .execute_ready_with_live(&engine, study, &mut first_lookup)
            .expect("initial execution")
    );
    assert_eq!(
        runtime
            .output_series(study.output(0))
            .expect("initial output")
            .values(),
        &[Some(108.0), Some(108.0)]
    );

    let second_live = StudyLiveMarketData::new(
        Some(StudyQuoteView::new(&live.quote_two, 2, 0)),
        Some(StudyTradeWindow::new(&live.trades, 1, 2, 2, 0)),
        Some(StudyDepthView::new(&live.book, 2, 0)),
    );
    let mut second_lookup = |_input: &StudyMarketInput| Some(second_live);
    let batch = runtime
        .execute_live_non_bar_change_with_live(
            &engine,
            StudyNonBarChange {
                provider_id: "provider",
                instrument_id: "ES",
                entitlement_id: "entitlement",
                stream: MarketStream::Quotes,
                observed_unix_nanos: event_metadata(4, 90)
                    .timestamps
                    .exchange_unix_nanos
                    .expect("exchange timestamp"),
            },
            &mut second_lookup,
            &mut |_| true,
        )
        .expect("quote change recalculates");
    assert_eq!(batch.executed, vec![study]);
    assert_eq!(batch.errors, [] as [StudyRuntimeError; 0]);
    assert_eq!(
        runtime
            .output_series(study.output(0))
            .expect("updated output")
            .values(),
        &[Some(108.0), Some(128.0)]
    );
}

#[test]
fn non_bar_change_updates_only_its_containing_row_and_ignores_outside_coverage() {
    let mut runtime = StudyRuntime::new(config(4));
    let mut engine = engine(1);
    let source = series("ES");
    engine
        .install_history(ProviderGeneration(NonZeroU64::MIN), &source, 2, 0, bars())
        .expect("canonical history installs");
    let streams = StreamRequirements::BARS
        .with(MarketStream::Trades)
        .with(MarketStream::Quotes)
        .with(MarketStream::Depth);
    let definition = definition(
        "live_microstructure_exact_row",
        vec![market(source, streams)],
        1,
        StudyInvalidationPolicy::SameRange,
    );
    let study = runtime
        .register_native_for_consumer(
            ConsumerId(NonZeroU64::MIN),
            NativeStudyRegistration {
                settings: StudySettings::defaults(&definition.settings).expect("defaults"),
                definition,
                program: NativeStudyProgram::stateless(calculate_live_microstructure),
            },
        )
        .expect("study registers");
    let live = live_microstructure_fixture();
    let first_live = StudyLiveMarketData::new(
        Some(StudyQuoteView::new(&live.quote_one, 2, 0)),
        Some(StudyTradeWindow::new(&live.trades, 1, 2, 2, 0)),
        Some(StudyDepthView::new(&live.book, 2, 0)),
    );
    let mut initial_lookup = |_input: &StudyMarketInput| Some(first_live);
    runtime
        .execute_ready_with_live(&engine, study, &mut initial_lookup)
        .expect("initial execution succeeds");

    let second_live = StudyLiveMarketData::new(
        Some(StudyQuoteView::new(&live.quote_two, 2, 0)),
        Some(StudyTradeWindow::new(&live.trades, 1, 2, 2, 0)),
        Some(StudyDepthView::new(&live.book, 2, 0)),
    );
    let mut changed_lookup = |_input: &StudyMarketInput| Some(second_live);
    let changed = runtime
        .execute_live_non_bar_change_with_live(
            &engine,
            StudyNonBarChange {
                provider_id: "provider",
                instrument_id: "ES",
                entitlement_id: "entitlement",
                stream: MarketStream::Quotes,
                observed_unix_nanos: event_metadata(4, 30)
                    .timestamps
                    .exchange_unix_nanos
                    .expect("exchange timestamp"),
            },
            &mut changed_lookup,
            &mut |_| true,
        )
        .expect("late first-bar quote recalculates");
    assert_eq!(changed.executed, vec![study]);
    assert_eq!(changed.errors, [] as [StudyRuntimeError; 0]);
    assert_eq!(
        runtime
            .output_series(study.output(0))
            .expect("updated output")
            .values(),
        &[Some(128.0), Some(108.0)]
    );

    for offset_seconds in [-1, 121] {
        let mut lookup = |_input: &StudyMarketInput| Some(second_live);
        let ignored = runtime
            .execute_live_non_bar_change_with_live(
                &engine,
                StudyNonBarChange {
                    provider_id: "provider",
                    instrument_id: "ES",
                    entitlement_id: "entitlement",
                    stream: MarketStream::Quotes,
                    observed_unix_nanos: event_metadata(5, offset_seconds)
                        .timestamps
                        .exchange_unix_nanos
                        .expect("exchange timestamp"),
                },
                &mut lookup,
                &mut |_| true,
            )
            .expect("out-of-coverage quote is ignored");
        assert_eq!(ignored.executed, [] as [StudyInstanceId; 0]);
        assert_eq!(ignored.errors, [] as [StudyRuntimeError; 0]);
    }
    assert_eq!(
        runtime
            .output_series(study.output(0))
            .expect("unchanged output")
            .values(),
        &[Some(128.0), Some(108.0)]
    );
}

#[test]
fn non_bar_change_inside_fixed_time_history_gap_is_ignored() {
    let mut runtime = StudyRuntime::new(config(4));
    let mut engine = engine(1);
    let source = series("ES-GAP");
    engine
        .install_history(
            ProviderGeneration(NonZeroU64::MIN),
            &source,
            2,
            0,
            vec![bar(1, 0, 10_500), bar(2, 180, 12_000)],
        )
        .expect("gapped canonical history installs");
    let streams = StreamRequirements::BARS
        .with(MarketStream::Trades)
        .with(MarketStream::Quotes)
        .with(MarketStream::Depth);
    let definition = definition(
        "live_microstructure_internal_gap",
        vec![market(source, streams)],
        1,
        StudyInvalidationPolicy::SameRange,
    );
    let study = runtime
        .register_native_for_consumer(
            ConsumerId(NonZeroU64::MIN),
            NativeStudyRegistration {
                settings: StudySettings::defaults(&definition.settings).expect("defaults"),
                definition,
                program: NativeStudyProgram::stateless(calculate_live_microstructure),
            },
        )
        .expect("study registers");
    let live = live_microstructure_fixture();
    let first_live = StudyLiveMarketData::new(
        Some(StudyQuoteView::new(&live.quote_one, 2, 0)),
        Some(StudyTradeWindow::new(&live.trades, 1, 2, 2, 0)),
        Some(StudyDepthView::new(&live.book, 2, 0)),
    );
    let mut initial_lookup = |_input: &StudyMarketInput| Some(first_live);
    runtime
        .execute_ready_with_live(&engine, study, &mut initial_lookup)
        .expect("initial execution succeeds");
    let committed = runtime
        .output_series(study.output(0))
        .expect("initial output")
        .clone();

    let second_live = StudyLiveMarketData::new(
        Some(StudyQuoteView::new(&live.quote_two, 2, 0)),
        Some(StudyTradeWindow::new(&live.trades, 1, 2, 2, 0)),
        Some(StudyDepthView::new(&live.book, 2, 0)),
    );
    let mut gap_lookup = |_input: &StudyMarketInput| Some(second_live);
    let gap_change = runtime
        .execute_live_non_bar_change_with_live(
            &engine,
            StudyNonBarChange {
                provider_id: "provider",
                instrument_id: "ES-GAP",
                entitlement_id: "entitlement",
                stream: MarketStream::Quotes,
                observed_unix_nanos: event_metadata(20, 120)
                    .timestamps
                    .exchange_unix_nanos
                    .expect("exchange timestamp"),
            },
            &mut gap_lookup,
            &mut |_| true,
        )
        .expect("internal-gap quote is ignored");
    assert_eq!(gap_change.executed, [] as [StudyInstanceId; 0]);
    assert_eq!(gap_change.errors, [] as [StudyRuntimeError; 0]);
    assert_eq!(runtime.output_series(study.output(0)), Some(&committed));

    let mut contained_lookup = |_input: &StudyMarketInput| Some(second_live);
    let contained = runtime
        .execute_live_non_bar_change_with_live(
            &engine,
            StudyNonBarChange {
                provider_id: "provider",
                instrument_id: "ES-GAP",
                entitlement_id: "entitlement",
                stream: MarketStream::Quotes,
                observed_unix_nanos: event_metadata(21, 210)
                    .timestamps
                    .exchange_unix_nanos
                    .expect("exchange timestamp"),
            },
            &mut contained_lookup,
            &mut |_| true,
        )
        .expect("event inside the later bar recalculates");
    assert_eq!(contained.executed, vec![study]);
    assert_eq!(contained.errors, [] as [StudyRuntimeError; 0]);
    assert_eq!(
        runtime
            .output_series(study.output(0))
            .expect("later bar output")
            .value(1),
        Some(Some(128.0))
    );
}

#[test]
fn ranged_history_change_reuses_state_and_recomputes_only_repaired_rows() {
    let mut runtime = StudyRuntime::new(config(4));
    let mut engine = engine(1);
    let source = series("ES");
    let generation = ProviderGeneration(NonZeroU64::MIN);
    engine
        .install_history(generation, &source, 2, 0, bars())
        .expect("canonical history installs");
    let study = runtime
        .register_native_for_consumer(
            ConsumerId(NonZeroU64::MIN),
            stateful_registration(source.clone(), 8),
        )
        .expect("stateful study registers");
    runtime
        .execute_ready(&engine, study)
        .expect("covering execution succeeds");
    assert_eq!(committed_test_state(&mut runtime, study).executions, 1);
    assert_eq!(
        runtime
            .output_series(study.output(0))
            .expect("initial output")
            .values(),
        &[Some(1.0), Some(1.0)]
    );

    let mut repaired = bars();
    repaired[1].close = 11_250;
    repaired[1].high = 11_750;
    let changed_timestamp = repaired[1].exchange_timestamp_unix_nanos;
    engine
        .replace_covering_history(generation, &source, 2, 0, repaired, true)
        .expect("history repair installs");
    let mut no_live_market = |_input: &StudyMarketInput| None;
    let batch = runtime
        .execute_history_range_change_with_live(
            &engine,
            &source,
            changed_timestamp,
            changed_timestamp,
            &mut no_live_market,
        )
        .expect("ranged repair executes incrementally");
    assert_eq!(batch.executed, vec![study]);
    assert_eq!(batch.errors, [] as [StudyRuntimeError; 0]);
    assert_eq!(committed_test_state(&mut runtime, study).executions, 2);
    assert_eq!(
        runtime
            .output_series(study.output(0))
            .expect("repaired output")
            .values(),
        &[Some(1.0), Some(2.0)]
    );
}

#[test]
fn native_reinitialization_preserves_identity_invalidates_downstream_and_reexecutes() {
    let mut fixture = native_chain(3);
    let owner = ConsumerId(NonZeroU64::MIN);

    let affected = fixture
        .runtime
        .reinitialize_native_for_consumer(
            owner,
            fixture.producer,
            scaled_close_registration(fixture.source.clone(), 5),
        )
        .expect("producer reinitializes in place");
    assert_eq!(affected, vec![fixture.producer, fixture.consumer]);
    assert!(
        fixture
            .runtime
            .output_series(fixture.producer.output(0))
            .is_none()
    );
    assert!(
        fixture
            .runtime
            .output_series(fixture.consumer.output(0))
            .is_none()
    );

    assert_eq!(
        fixture
            .runtime
            .execute_ready_subtree(&fixture.engine, fixture.producer)
            .expect("reinitialized subtree executes"),
        vec![fixture.producer, fixture.consumer]
    );
    assert_eq!(
        fixture
            .runtime
            .output_series(fixture.producer.output(0))
            .expect("producer output")
            .values(),
        &[Some(52_500.0), Some(55_000.0)]
    );
    assert_eq!(
        fixture
            .runtime
            .output_series(fixture.consumer.output(0))
            .expect("consumer output")
            .values(),
        &[Some(105_000.0), Some(110_000.0)]
    );
}

#[test]
fn native_reinitialization_rejects_output_interface_changes_and_dependency_cycles() {
    let mut fixture = native_chain(3);
    let owner = ConsumerId(NonZeroU64::MIN);
    let original_registration = fixture
        .runtime
        .native_registration(fixture.producer)
        .expect("producer registration is retained");
    let mut changed_interface = scaled_close_registration(fixture.source.clone(), 5);
    changed_interface.definition.outputs[0].identifier = "replacement".to_string();
    assert_eq!(
        fixture
            .runtime
            .reinitialize_native_for_consumer(owner, fixture.producer, changed_interface)
            .expect_err("output identities stay stable"),
        StudyRuntimeError::OutputInterfaceChanged(fixture.producer)
    );
    assert!(
        fixture
            .runtime
            .output_series(fixture.producer.output(0))
            .is_some()
    );
    let retained_registration = fixture
        .runtime
        .native_registration(fixture.producer)
        .expect("failed reinitialization preserves the original registration");
    assert_eq!(
        retained_registration.definition,
        original_registration.definition
    );
    assert_eq!(
        retained_registration.settings,
        original_registration.settings
    );

    let mut self_cycle = fixture
        .runtime
        .native_registration(fixture.consumer)
        .expect("consumer registration is retained");
    self_cycle.definition.dependencies = vec![StudyDependency::Output(fixture.consumer.output(0))];
    assert_eq!(
        fixture
            .runtime
            .reinitialize_native_for_consumer(owner, fixture.consumer, self_cycle)
            .expect_err("self dependency is a cycle"),
        StudyRuntimeError::DependencyOrderViolation {
            study_id: fixture.consumer,
            dependency: fixture.consumer.output(0),
        }
    );
}

#[test]
fn live_append_and_revision_recompute_only_timestamp_mapped_rows_through_the_dag() {
    let mut fixture = native_chain(3);

    let appended = MarketBar {
        source_sequence: 3,
        exchange_timestamp_seconds: 1_700_000_120,
        exchange_timestamp_unix_nanos: 1_700_000_120_000_000_000,
        open: 11_000,
        high: 12_000,
        low: 10_500,
        close: 11_500,
        volume: 1_750,
    };
    fixture
        .engine
        .install_realtime_tail(
            fixture.provider_generation,
            &fixture.source,
            2,
            3,
            appended,
            true,
        )
        .expect("canonical append installs");
    assert_eq!(
        fixture
            .runtime
            .execute_live_market_change(
                &fixture.engine,
                &fixture.source,
                appended.exchange_timestamp_unix_nanos,
            )
            .expect("append recalculates"),
        vec![fixture.producer, fixture.consumer]
    );
    let producer_id = StudyOutputId {
        study_id: fixture.producer,
        output_index: 0,
    };
    let consumer_id = StudyOutputId {
        study_id: fixture.consumer,
        output_index: 0,
    };
    assert_eq!(
        fixture
            .runtime
            .output_series(producer_id)
            .expect("producer")
            .values(),
        &[Some(31_500.0), Some(33_000.0), Some(34_500.0)]
    );
    assert_eq!(
        fixture
            .runtime
            .output_series(consumer_id)
            .expect("consumer")
            .values(),
        &[Some(63_000.0), Some(66_000.0), Some(69_000.0)]
    );

    let revised = MarketBar {
        close: 12_000,
        high: 12_250,
        ..appended
    };
    fixture
        .engine
        .install_realtime_tail(
            fixture.provider_generation,
            &fixture.source,
            2,
            3,
            revised,
            true,
        )
        .expect("canonical revision installs");
    assert_eq!(
        fixture
            .runtime
            .execute_live_market_change(
                &fixture.engine,
                &fixture.source,
                revised.exchange_timestamp_unix_nanos,
            )
            .expect("revision recalculates"),
        vec![fixture.producer, fixture.consumer]
    );
    assert_eq!(
        fixture
            .runtime
            .output_series(producer_id)
            .expect("producer")
            .values(),
        &[Some(31_500.0), Some(33_000.0), Some(36_000.0)]
    );
    assert_eq!(
        fixture
            .runtime
            .output_series(consumer_id)
            .expect("consumer")
            .values(),
        &[Some(63_000.0), Some(66_000.0), Some(72_000.0)]
    );
}

#[test]
fn failing_studies_do_not_starve_independent_work_or_advance_dependents() {
    let mut fixture = failure_isolation_fixture();
    let revised = bar(3, 120, 12_000);
    fixture
        .engine
        .install_realtime_tail(fixture.generation, &fixture.source, 2, 3, revised, true)
        .expect("canonical append installs");
    let mut no_live_market = |_input: &StudyMarketInput| None;
    let batch = fixture
        .runtime
        .execute_live_market_change_with_live(
            &fixture.engine,
            &fixture.source,
            revised.exchange_timestamp_unix_nanos,
            &mut no_live_market,
        )
        .expect("execution wave remains structurally valid");

    assert_eq!(batch.executed, vec![fixture.independent]);
    assert_eq!(batch.errors.len(), 2);
    assert!(matches!(
        &batch.errors[0],
        StudyRuntimeError::ExecutionRejected { study_id, .. }
            if *study_id == fixture.first_failing
    ));
    assert!(matches!(
        &batch.errors[1],
        StudyRuntimeError::ExecutionRejected { study_id, .. }
            if *study_id == fixture.second_failing
    ));
    assert_eq!(
        output_generation(&fixture.runtime, fixture.first_failing),
        fixture.generations[0]
    );
    assert_eq!(
        output_generation(&fixture.runtime, fixture.dependent),
        fixture.generations[1]
    );
    assert_eq!(
        output_generation(&fixture.runtime, fixture.second_failing),
        fixture.generations[2]
    );
    let independent_output = fixture
        .runtime
        .output_series(fixture.independent.output(0))
        .expect("independent output advances");
    assert!(independent_output.generation() > fixture.generations[3]);
    assert_eq!(independent_output.value(2), Some(Some(48_000.0)));
}

#[test]
fn large_history_tail_execution_preparation_is_bounded_by_dirty_rows_and_outputs() {
    let mut fixture = large_history_fixture();
    assert_eq!(
        fixture.runtime.last_output_preparation_work_rows,
        LARGE_HISTORY_ROWS * LARGE_OUTPUT_COUNT
    );

    let before_append = fixture
        .runtime
        .output_series(fixture.study.output(0))
        .expect("pre-append output")
        .clone();
    let old_tail = before_append
        .value(LARGE_HISTORY_ROWS - 1)
        .expect("pre-append tail row");
    let appended = bar(
        u64::try_from(LARGE_HISTORY_ROWS + 1).expect("append sequence"),
        i64::try_from(LARGE_HISTORY_ROWS).expect("append offset") * 60,
        fixture.last_history_bar.close + 250,
    );
    fixture
        .engine
        .install_realtime_tail(fixture.generation, &fixture.source, 2, 0, appended, true)
        .expect("large-history append installs");
    assert_eq!(
        fixture
            .runtime
            .execute_live_market_change(
                &fixture.engine,
                &fixture.source,
                appended.exchange_timestamp_unix_nanos,
            )
            .expect("large-history append executes"),
        vec![fixture.study]
    );
    assert_eq!(
        fixture.runtime.last_output_preparation_work_rows,
        LARGE_OUTPUT_COUNT
    );
    assert_eq!(
        before_append.value(LARGE_HISTORY_ROWS - 1),
        Some(old_tail),
        "the previously published full snapshot stays stable"
    );
    let appended_output = fixture
        .runtime
        .output_series(fixture.study.output(0))
        .expect("appended full output snapshot");
    assert_eq!(appended_output.len(), LARGE_HISTORY_ROWS + 1);
    assert_eq!(
        appended_output.value(LARGE_HISTORY_ROWS),
        Some(Some(f64::from(
            i32::try_from(appended.close * 2).expect("bounded appended close")
        )))
    );

    let before_revision = appended_output.clone();
    let revised = MarketBar {
        close: appended.close + 500,
        high: appended.high + 500,
        ..appended
    };
    fixture
        .engine
        .install_realtime_tail(fixture.generation, &fixture.source, 2, 0, revised, true)
        .expect("large-history tail revision installs");
    assert_eq!(
        fixture
            .runtime
            .execute_live_market_change(
                &fixture.engine,
                &fixture.source,
                revised.exchange_timestamp_unix_nanos,
            )
            .expect("large-history tail revision executes"),
        vec![fixture.study]
    );
    assert_eq!(
        fixture.runtime.last_output_preparation_work_rows,
        LARGE_OUTPUT_COUNT
    );
    assert_eq!(before_revision.len(), LARGE_HISTORY_ROWS + 1);
    assert_eq!(
        before_revision.value(LARGE_HISTORY_ROWS),
        Some(Some(f64::from(
            i32::try_from(appended.close * 2).expect("bounded appended close")
        )))
    );
    let revised_output = fixture
        .runtime
        .output_series(fixture.study.output(0))
        .expect("revised full output snapshot");
    assert_eq!(revised_output.len(), LARGE_HISTORY_ROWS + 1);
    assert_eq!(
        revised_output.value(LARGE_HISTORY_ROWS),
        Some(Some(f64::from(
            i32::try_from(revised.close * 2).expect("bounded revised close")
        )))
    );
}

#[test]
fn output_primary_mtf_incremental_mapping_does_not_materialize_large_timestamps() {
    let mut fixture = large_history_fixture();
    let secondary = series_period("ES-LARGE-5M", 300);
    let secondary_start = i64::try_from(LARGE_HISTORY_ROWS - 6).expect("secondary start") * 60;
    fixture
        .engine
        .install_history(
            fixture.generation,
            &secondary,
            2,
            0,
            vec![bar(1, secondary_start, 20_000)],
        )
        .expect("secondary large-history series installs");

    let consumer_definition = definition(
        "output_primary_large_mtf",
        vec![
            StudyDependency::Output(fixture.study.output(0)),
            market(secondary.clone(), StreamRequirements::BARS),
        ],
        1,
        StudyInvalidationPolicy::SameRange,
    );
    let consumer = fixture
        .runtime
        .register_native_for_consumer(
            ConsumerId(NonZeroU64::MIN),
            NativeStudyRegistration {
                settings: StudySettings::defaults(&consumer_definition.settings)
                    .expect("consumer defaults"),
                definition: consumer_definition,
                program: NativeStudyProgram::stateless(calculate_double_output),
            },
        )
        .expect("output-primary consumer registers");
    fixture
        .runtime
        .execute_ready(&fixture.engine, consumer)
        .expect("output-primary consumer covering execution succeeds");

    let producer_output = fixture
        .runtime
        .output_series(fixture.study.output(0))
        .expect("producer output exists");
    assert!(
        producer_output.timeline.0.materialized.get().is_none(),
        "covering execution does not require the public contiguous timestamp view"
    );
    let committed_consumer = fixture
        .runtime
        .output_series(consumer.output(0))
        .expect("consumer output exists")
        .clone();
    assert_eq!(committed_consumer.len(), LARGE_HISTORY_ROWS);

    let secondary_tail = bar(
        2,
        i64::try_from(LARGE_HISTORY_ROWS - 1).expect("secondary tail offset") * 60,
        21_000,
    );
    fixture
        .engine
        .install_realtime_tail(fixture.generation, &secondary, 2, 0, secondary_tail, true)
        .expect("secondary tail installs");
    assert_eq!(
        fixture
            .runtime
            .execute_live_market_change(
                &fixture.engine,
                &secondary,
                secondary_tail.exchange_timestamp_unix_nanos,
            )
            .expect("secondary output-primary change executes"),
        vec![consumer]
    );
    assert_eq!(
        fixture.runtime.last_output_preparation_work_rows, 1,
        "only the mapped primary tail row is prepared"
    );
    assert!(
        fixture
            .runtime
            .output_series(fixture.study.output(0))
            .expect("producer output remains installed")
            .timeline
            .0
            .materialized
            .get()
            .is_none(),
        "incremental output-primary timestamp mapping must stay on the canonical bar index"
    );
    assert_eq!(
        committed_consumer.len(),
        LARGE_HISTORY_ROWS,
        "the previously published full snapshot remains stable"
    );
    let current_consumer = fixture
        .runtime
        .output_series(consumer.output(0))
        .expect("current consumer output");
    assert_eq!(current_consumer.len(), LARGE_HISTORY_ROWS);
    assert!(current_consumer.generation() > committed_consumer.generation());
}

#[test]
fn secondary_timeframe_changes_map_to_primary_rows_by_timestamp_not_row_index() {
    let mut fixture = mtf_fixture();
    assert_eq!(
        fixture
            .runtime
            .output_series(fixture.output)
            .expect("MTF output")
            .values(),
        &[Some(20_000.0); 7]
    );

    let secondary_append = bar(2, 300, 21_000);
    fixture
        .engine
        .install_realtime_tail(
            fixture.provider_generation,
            &fixture.secondary,
            2,
            3,
            secondary_append,
            true,
        )
        .expect("secondary append installs");
    assert_eq!(
        fixture
            .runtime
            .execute_live_market_change(
                &fixture.engine,
                &fixture.secondary,
                secondary_append.exchange_timestamp_unix_nanos,
            )
            .expect("secondary append recalculates"),
        vec![fixture.study]
    );
    assert_eq!(
        fixture
            .runtime
            .output_series(fixture.output)
            .expect("MTF output")
            .values(),
        &[
            Some(20_000.0),
            Some(20_000.0),
            Some(20_000.0),
            Some(20_000.0),
            Some(20_000.0),
            Some(21_000.0),
            Some(21_000.0),
        ]
    );

    let secondary_revision = MarketBar {
        close: 22_000,
        high: 22_100,
        low: 21_900,
        open: 22_000,
        ..secondary_append
    };
    fixture
        .engine
        .install_realtime_tail(
            fixture.provider_generation,
            &fixture.secondary,
            2,
            3,
            secondary_revision,
            true,
        )
        .expect("secondary revision installs");
    fixture
        .runtime
        .execute_live_market_change(
            &fixture.engine,
            &fixture.secondary,
            secondary_revision.exchange_timestamp_unix_nanos,
        )
        .expect("secondary revision recalculates");
    assert_eq!(
        fixture
            .runtime
            .output_series(fixture.output)
            .expect("MTF output")
            .values(),
        &[
            Some(20_000.0),
            Some(20_000.0),
            Some(20_000.0),
            Some(20_000.0),
            Some(20_000.0),
            Some(22_000.0),
            Some(22_000.0),
        ]
    );
}

#[test]
fn slow_native_calculation_cannot_commit_late_or_start_replacement_workers() {
    fn delayed(context: &mut StudyExecutionContext<'_>) -> Result<(), String> {
        std::thread::sleep(std::time::Duration::from_millis(150));
        calculate_scaled_close(context)
    }
    let mut fixture = native_chain(2);
    fixture
        .runtime
        .execute_ready(&fixture.engine, fixture.producer)
        .unwrap();
    let output = StudyOutputId {
        study_id: fixture.producer,
        output_index: 0,
    };
    let committed = fixture.runtime.output_series(output).unwrap().clone();
    fixture
        .runtime
        .studies
        .get_mut(&fixture.producer)
        .unwrap()
        .program = Some(NativeStudyProgram::stateless(delayed));
    assert!(
        matches!(fixture.runtime.execute_ready(&fixture.engine, fixture.producer),
            Err(StudyRuntimeError::ExecutionRejected { detail, .. }) if detail.contains("deadline"))
    );
    // A subsequent call rejects admission while the original worker is busy.
    assert!(
        matches!(fixture.runtime.execute_ready(&fixture.engine, fixture.producer),
            Err(StudyRuntimeError::ExecutionRejected { detail, .. }) if detail.contains("disabled"))
    );
    std::thread::sleep(std::time::Duration::from_millis(160));
    assert_eq!(fixture.runtime.output_series(output), Some(&committed));
}

#[test]
fn rejected_native_execution_preserves_the_last_committed_output() {
    let mut runtime = StudyRuntime::new(config(4));
    let mut engine = engine(1);
    let source = series("ES");
    engine
        .install_history(ProviderGeneration(NonZeroU64::MIN), &source, 2, 3, bars())
        .expect("canonical history installs");
    let study = runtime
        .register_native_for_consumer(
            ConsumerId(NonZeroU64::MIN),
            scaled_close_registration(source, 2),
        )
        .expect("study registers");
    assert!(
        runtime
            .execute_ready(&engine, study)
            .expect("first execution")
    );
    let output_id = StudyOutputId {
        study_id: study,
        output_index: 0,
    };
    let committed = runtime
        .output_series(output_id)
        .expect("first output")
        .clone();
    runtime
        .studies
        .get_mut(&study)
        .expect("study remains live")
        .program = Some(NativeStudyProgram {
        calculate: calculate_then_fail,
        state_factory: None,
    });

    assert!(matches!(
        runtime.execute_ready(&engine, study),
        Err(StudyRuntimeError::ExecutionRejected { study_id, .. }) if study_id == study
    ));
    assert_eq!(runtime.output_series(output_id), Some(&committed));
}

#[test]
fn stateful_execution_commits_incrementally_and_covering_execution_resets_state() {
    let mut runtime = StudyRuntime::new(config(4));
    let mut engine = engine(1);
    let source = series("ES");
    engine
        .install_history(ProviderGeneration(NonZeroU64::MIN), &source, 2, 3, bars())
        .expect("canonical history installs");
    let study = runtime
        .register_native_for_consumer(
            ConsumerId(NonZeroU64::MIN),
            stateful_registration(source, 8),
        )
        .expect("stateful study registers");

    assert!(
        runtime
            .execute_ready(&engine, study)
            .expect("full execution")
    );
    assert_eq!(committed_test_state(&mut runtime, study).executions, 1);
    assert_eq!(
        runtime
            .output_series(study.output(0))
            .expect("stateful output")
            .values(),
        &[Some(1.0), Some(1.0)]
    );

    assert!(
        runtime
            .execute_ready_range(
                &engine,
                study,
                Some(StudyDirtyRange::bounded(1, 2).expect("dirty row")),
            )
            .expect("incremental execution")
    );
    assert_eq!(committed_test_state(&mut runtime, study).executions, 2);
    assert_eq!(
        runtime
            .output_series(study.output(0))
            .expect("incremental output")
            .values(),
        &[Some(1.0), Some(2.0)]
    );

    assert!(
        runtime
            .execute_ready_range(
                &engine,
                study,
                Some(StudyDirtyRange::bounded(0, 2).expect("covering dirty range")),
            )
            .expect("explicit covering execution")
    );
    assert_eq!(committed_test_state(&mut runtime, study).executions, 1);
    assert_eq!(
        runtime
            .output_series(study.output(0))
            .expect("rebuilt output")
            .values(),
        &[Some(1.0), Some(1.0)]
    );
}

#[test]
fn rejected_and_panicking_stateful_execution_preserve_state_output_and_generation() {
    let mut runtime = StudyRuntime::new(config(4));
    let mut engine = engine(1);
    let source = series("ES");
    engine
        .install_history(ProviderGeneration(NonZeroU64::MIN), &source, 2, 3, bars())
        .expect("canonical history installs");
    let study = runtime
        .register_native_for_consumer(
            ConsumerId(NonZeroU64::MIN),
            stateful_registration(source, 8),
        )
        .expect("stateful study registers");
    runtime
        .execute_ready(&engine, study)
        .expect("initial state commits");
    let committed_output = runtime
        .output_series(study.output(0))
        .expect("stateful output")
        .clone();
    let committed_generation = committed_output.generation();

    runtime
        .studies
        .get_mut(&study)
        .and_then(|node| node.program.as_mut())
        .expect("native program")
        .calculate = calculate_stateful_then_fail;
    assert!(matches!(
        runtime.execute_ready_range(
            &engine,
            study,
            Some(StudyDirtyRange::bounded(0, 1).expect("dirty row")),
        ),
        Err(StudyRuntimeError::ExecutionRejected { study_id, .. }) if study_id == study
    ));
    assert_eq!(committed_test_state(&mut runtime, study).executions, 1);
    assert_eq!(
        runtime.output_series(study.output(0)),
        Some(&committed_output)
    );
    assert_eq!(
        runtime
            .output_series(study.output(0))
            .expect("committed output")
            .generation(),
        committed_generation
    );

    runtime
        .studies
        .get_mut(&study)
        .and_then(|node| node.program.as_mut())
        .expect("native program")
        .calculate = calculate_stateful_then_panic;
    assert!(matches!(
        runtime.execute_ready_range(
            &engine,
            study,
            Some(StudyDirtyRange::bounded(0, 1).expect("dirty row")),
        ),
        Err(StudyRuntimeError::ExecutionRejected { study_id, .. }) if study_id == study
    ));
    assert_eq!(committed_test_state(&mut runtime, study).executions, 1);
    assert_eq!(
        runtime.output_series(study.output(0)),
        Some(&committed_output)
    );
}

#[test]
fn shared_interior_state_candidate_failure_cannot_mutate_committed_state() {
    let mut runtime = StudyRuntime::new(config(4));
    let mut engine = engine(1);
    let source = series("ES");
    engine
        .install_history(ProviderGeneration(NonZeroU64::MIN), &source, 2, 3, bars())
        .expect("canonical history installs");
    let definition = definition(
        "shared_transactional_state",
        vec![market(source, StreamRequirements::BARS)],
        1,
        StudyInvalidationPolicy::SameRange,
    );
    let study = runtime
        .register_native_for_consumer(
            ConsumerId(NonZeroU64::MIN),
            NativeStudyRegistration {
                settings: StudySettings::defaults(&definition.settings)
                    .expect("shared-state defaults"),
                definition,
                program: NativeStudyProgram::stateful(
                    calculate_shared_transactional_state,
                    create_shared_transactional_state,
                ),
            },
        )
        .expect("shared-state study registers");

    runtime
        .execute_ready(&engine, study)
        .expect("covering shared-state execution succeeds");
    let committed_output = runtime
        .output_series(study.output(0))
        .expect("committed shared-state output")
        .clone();
    assert_eq!(committed_shared_state_executions(&runtime, study), 1);

    assert!(matches!(
        runtime.execute_ready_range(
            &engine,
            study,
            Some(StudyDirtyRange::bounded(1, 2).expect("incremental dirty row")),
        ),
        Err(StudyRuntimeError::ExecutionRejected { study_id, .. }) if study_id == study
    ));
    assert_eq!(committed_shared_state_executions(&runtime, study), 1);
    assert_eq!(
        runtime.output_series(study.output(0)),
        Some(&committed_output)
    );
}

#[test]
fn state_growth_beyond_limit_is_transactional() {
    let mut runtime = StudyRuntime::new(config(4));
    let mut engine = engine(1);
    let source = series("ES");
    engine
        .install_history(ProviderGeneration(NonZeroU64::MIN), &source, 2, 3, bars())
        .expect("canonical history installs");
    let study = runtime
        .register_native_for_consumer(
            ConsumerId(NonZeroU64::MIN),
            stateful_registration(source, 8),
        )
        .expect("stateful study registers");
    runtime
        .execute_ready(&engine, study)
        .expect("initial state commits");
    let committed_output = runtime
        .output_series(study.output(0))
        .expect("stateful output")
        .clone();
    runtime
        .studies
        .get_mut(&study)
        .and_then(|node| node.program.as_mut())
        .expect("native program")
        .calculate = calculate_stateful_growth;

    assert_eq!(
        runtime
            .execute_ready_range(
                &engine,
                study,
                Some(StudyDirtyRange::bounded(0, 1).expect("dirty row")),
            )
            .expect_err("oversized candidate state must not commit"),
        StudyRuntimeError::StateMemoryLimitExceeded {
            maximum: 1_024,
            requested: 2_048,
        }
    );
    assert_eq!(committed_test_state(&mut runtime, study).executions, 1);
    assert_eq!(committed_test_state(&mut runtime, study).accounted_bytes, 8);
    assert_eq!(
        runtime.output_series(study.output(0)),
        Some(&committed_output)
    );
    assert_eq!(runtime.state_bytes, 8);
}

#[test]
fn state_memory_bounds_and_removal_accounting_are_enforced() {
    let mut runtime = StudyRuntime::new(StudyRuntimeConfig {
        maximum_studies: bound(4),
        maximum_dependencies_per_study: bound(2),
        maximum_outputs_per_study: bound(2),
        maximum_points_per_output: bound(64),
        maximum_total_output_points: bound(256),
        maximum_state_bytes_per_study: bound(8),
        maximum_total_state_bytes: bound(12),
    });
    let owner = ConsumerId(NonZeroU64::MIN);
    assert_eq!(
        runtime
            .register_native_for_consumer(owner, stateful_registration(series("ES"), 9))
            .expect_err("per-study state bound"),
        StudyRuntimeError::StateMemoryLimitExceeded {
            maximum: 8,
            requested: 9,
        }
    );
    let es = runtime
        .register_native_for_consumer(owner, stateful_registration(series("ES"), 4))
        .expect("first state fits");
    runtime
        .register_native_for_consumer(owner, stateful_registration(series("NQ"), 8))
        .expect("second state exactly fills total budget");
    assert_eq!(runtime.state_bytes, 12);
    assert_eq!(
        runtime
            .register_native_for_consumer(owner, stateful_registration(series("YM"), 1))
            .expect_err("global state bound"),
        StudyRuntimeError::TotalStateMemoryLimitExceeded {
            maximum: 12,
            requested: 13,
        }
    );

    assert_eq!(
        runtime
            .remove_subtree(es)
            .expect("stateful subtree removes"),
        vec![es]
    );
    assert_eq!(runtime.state_bytes, 8);
    runtime
        .register_native_for_consumer(owner, stateful_registration(series("YM"), 1))
        .expect("released state budget is reusable");
    assert_eq!(runtime.state_bytes, 9);
}

#[test]
fn panicking_state_memory_accounting_is_rejected_without_unwinding() {
    let mut runtime = StudyRuntime::new(config(4));
    let mut registration = stateful_registration(series("ES"), 8);
    registration.program.state_factory = Some(create_panicking_accounted_state);

    assert_eq!(
        runtime
            .register_native_for_consumer(ConsumerId(NonZeroU64::MIN), registration)
            .expect_err("accounting panic must be contained"),
        StudyRuntimeError::StateInitializationRejected {
            detail: "native study state accounting panicked".to_string(),
        }
    );
    assert!(runtime.is_empty());
    assert_eq!(runtime.state_bytes, 0);
}

#[test]
fn reinitialization_resets_state_for_the_entire_dependent_subtree() {
    let mut runtime = StudyRuntime::new(config(4));
    let mut engine = engine(1);
    let source = series("ES");
    engine
        .install_history(ProviderGeneration(NonZeroU64::MIN), &source, 2, 3, bars())
        .expect("canonical history installs");
    let owner = ConsumerId(NonZeroU64::MIN);
    let root = runtime
        .register_native_for_consumer(owner, stateful_registration(source.clone(), 8))
        .expect("root registers");
    let dependent = runtime
        .register_native_for_consumer(
            owner,
            stateful_registration_with_dependency(StudyDependency::Output(root.output(0)), 8),
        )
        .expect("dependent registers");
    runtime
        .execute_ready_subtree(&engine, root)
        .expect("initial subtree execution");
    assert_eq!(committed_test_state(&mut runtime, root).executions, 1);
    assert_eq!(committed_test_state(&mut runtime, dependent).executions, 1);

    runtime
        .execute_ready_range(
            &engine,
            dependent,
            Some(StudyDirtyRange::bounded(0, 1).expect("dirty row")),
        )
        .expect("dependent incremental execution");
    assert_eq!(committed_test_state(&mut runtime, dependent).executions, 2);

    let affected = runtime
        .reinitialize_native_for_consumer(owner, root, stateful_registration(source, 8))
        .expect("root reinitializes");
    assert_eq!(affected, vec![root, dependent]);
    assert_eq!(committed_test_state(&mut runtime, root).executions, 0);
    assert_eq!(committed_test_state(&mut runtime, dependent).executions, 0);
    assert!(runtime.output_series(root.output(0)).is_none());
    assert!(runtime.output_series(dependent.output(0)).is_none());

    runtime
        .execute_ready_subtree(&engine, root)
        .expect("reinitialized subtree executes");
    assert_eq!(committed_test_state(&mut runtime, root).executions, 1);
    assert_eq!(committed_test_state(&mut runtime, dependent).executions, 1);
}

#[test]
fn subtree_checkpoint_restores_state_outputs_without_rewinding_generation() {
    let mut runtime = StudyRuntime::new(config(4));
    let mut engine = engine(1);
    let source = series("ES");
    engine
        .install_history(ProviderGeneration(NonZeroU64::MIN), &source, 2, 3, bars())
        .expect("canonical history installs");
    let owner = ConsumerId(NonZeroU64::MIN);
    let root = runtime
        .register_native_for_consumer(owner, stateful_registration(source, 8))
        .expect("root registers");
    let dependent = runtime
        .register_native_for_consumer(
            owner,
            stateful_registration_with_dependency(StudyDependency::Output(root.output(0)), 8),
        )
        .expect("dependent registers");
    runtime
        .execute_ready_subtree(&engine, root)
        .expect("initial subtree execution");
    let root_output = runtime
        .output_series(root.output(0))
        .expect("root output")
        .clone();
    let dependent_output = runtime
        .output_series(dependent.output(0))
        .expect("dependent output")
        .clone();
    let checkpoint = runtime
        .checkpoint_subtree(root)
        .expect("subtree checkpoint");
    let generation_before_mutation = runtime.next_output_generation;

    runtime
        .execute_ready_range(
            &engine,
            root,
            Some(StudyDirtyRange::bounded(0, 1).expect("dirty row")),
        )
        .expect("root incremental execution");
    runtime
        .execute_ready_range(
            &engine,
            dependent,
            Some(StudyDirtyRange::bounded(0, 1).expect("dirty row")),
        )
        .expect("dependent incremental execution");
    let generation_after_mutation = runtime.next_output_generation;
    assert!(generation_after_mutation > generation_before_mutation);
    assert_eq!(committed_test_state(&mut runtime, root).executions, 2);
    assert_eq!(committed_test_state(&mut runtime, dependent).executions, 2);

    runtime.restore_subtree_checkpoint(checkpoint);
    assert_eq!(committed_test_state(&mut runtime, root).executions, 1);
    assert_eq!(committed_test_state(&mut runtime, dependent).executions, 1);
    assert_eq!(runtime.output_series(root.output(0)), Some(&root_output));
    assert_eq!(
        runtime.output_series(dependent.output(0)),
        Some(&dependent_output)
    );
    assert_eq!(runtime.next_output_generation, generation_after_mutation);
}

#[test]
fn native_outputs_obey_per_output_and_global_point_bounds() {
    let mut runtime = StudyRuntime::new(StudyRuntimeConfig {
        maximum_studies: bound(2),
        maximum_dependencies_per_study: bound(2),
        maximum_outputs_per_study: bound(2),
        maximum_points_per_output: bound(1),
        maximum_total_output_points: bound(2),
        maximum_state_bytes_per_study: bound(1_024),
        maximum_total_state_bytes: bound(2_048),
    });
    let mut engine = engine(1);
    let source = series("ES");
    engine
        .install_history(ProviderGeneration(NonZeroU64::MIN), &source, 2, 3, bars())
        .expect("canonical history installs");
    let study = runtime
        .register_native_for_consumer(
            ConsumerId(NonZeroU64::MIN),
            scaled_close_registration(source, 1),
        )
        .expect("study registers");
    assert_eq!(
        runtime
            .execute_ready(&engine, study)
            .expect_err("two canonical rows exceed one-row output bound"),
        StudyRuntimeError::OutputPointLimitExceeded {
            maximum: 1,
            requested: 2,
        }
    );
    assert!(
        runtime
            .output_series(StudyOutputId {
                study_id: study,
                output_index: 0,
            })
            .is_none()
    );
}

#[test]
fn shared_market_requirements_union_without_owning_provider_work() {
    let mut runtime = StudyRuntime::new(config(8));
    let source = series("ES");
    runtime
        .register(definition(
            "close_sma",
            vec![market(source.clone(), StreamRequirements::BARS)],
            1,
            StudyInvalidationPolicy::TrailingWindow { bars: bound(20) },
        ))
        .expect("first study registers");
    runtime
        .register(definition(
            "trade_pressure",
            vec![market(
                source.clone(),
                StreamRequirements::BARS.with(MarketStream::Trades),
            )],
            1,
            StudyInvalidationPolicy::SameRange,
        ))
        .expect("second study registers");

    let requirements = runtime.market_requirements();
    assert_eq!(requirements.len(), 1);
    let streams = requirements.get(&source).expect("shared market request");
    assert!(streams.contains(MarketStream::Bars));
    assert!(streams.contains(MarketStream::Trades));
    assert!(!streams.contains(MarketStream::Depth));
}

#[test]
fn study_input_requirements_include_transitive_trade_and_depth_bindings() {
    let mut runtime = StudyRuntime::new(config(8));
    let source = series("ES");
    let upstream = runtime
        .register(definition(
            "microstructure",
            vec![market(
                source,
                StreamRequirements::BARS
                    .with(MarketStream::Trades)
                    .with(MarketStream::Depth),
            )],
            1,
            StudyInvalidationPolicy::SameRange,
        ))
        .expect("microstructure study registers");
    let downstream = runtime
        .register(definition(
            "smoothed_microstructure",
            vec![StudyDependency::Output(upstream.output(0))],
            1,
            StudyInvalidationPolicy::TrailingWindow { bars: bound(20) },
        ))
        .expect("downstream study registers");

    let requirements = runtime
        .input_stream_requirements(downstream)
        .expect("downstream requirements exist");
    assert!(requirements.contains(MarketStream::Bars));
    assert!(requirements.contains(MarketStream::Trades));
    assert!(requirements.contains(MarketStream::Depth));
    assert!(!requirements.contains(MarketStream::Quotes));
}

#[test]
fn study_market_dependencies_reconcile_to_one_engine_lease_per_series() {
    let mut runtime = StudyRuntime::new(config(8));
    let mut engine = engine(4);
    let source = series("ES");
    let bars = runtime
        .register(definition(
            "bars",
            vec![market(source.clone(), StreamRequirements::BARS)],
            1,
            StudyInvalidationPolicy::SameRange,
        ))
        .expect("bars study registers");
    let pressure = runtime
        .register(definition(
            "pressure",
            vec![market(
                source.clone(),
                StreamRequirements::BARS.with(MarketStream::Trades),
            )],
            1,
            StudyInvalidationPolicy::SameRange,
        ))
        .expect("pressure study registers");

    let changes = runtime
        .reconcile_market_leases(&mut engine)
        .expect("study demand reconciles");
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].kind, StudyMarketLeaseChangeKind::Acquired);
    let lease_id = changes[0].lease_id;
    let shared = StreamRequirements::BARS.with(MarketStream::Trades);
    assert_eq!(engine.data_lease_count(), 1);
    assert_eq!(engine.data_lease(lease_id), Some((&source, shared)));
    assert_eq!(
        engine
            .subscription_status(&source)
            .map(|status| status.consumer_count),
        Some(0)
    );
    assert_eq!(
        engine
            .subscription_status(&source)
            .map(|status| status.streams),
        Some(shared)
    );

    assert_eq!(
        runtime.remove_subtree(pressure).expect("pressure removes"),
        vec![pressure]
    );
    let changes = runtime
        .reconcile_market_leases(&mut engine)
        .expect("streams narrow in place");
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].lease_id, lease_id);
    assert_eq!(changes[0].kind, StudyMarketLeaseChangeKind::Updated);
    assert_eq!(
        engine.data_lease(lease_id),
        Some((&source, StreamRequirements::BARS))
    );

    assert_eq!(
        runtime.remove_subtree(bars).expect("bars removes"),
        vec![bars]
    );
    let changes = runtime
        .reconcile_market_leases(&mut engine)
        .expect("last study releases demand");
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].lease_id, lease_id);
    assert_eq!(changes[0].kind, StudyMarketLeaseChangeKind::Released);
    assert_eq!(engine.data_lease_count(), 0);
    assert!(!engine.has_subscription(&source));
}

#[test]
fn study_market_lease_capacity_preflight_does_not_partially_mutate_engine() {
    let mut runtime = StudyRuntime::new(config(8));
    let mut engine = engine(1);
    runtime
        .register(definition(
            "es",
            vec![market(series("ES"), StreamRequirements::BARS)],
            1,
            StudyInvalidationPolicy::SameRange,
        ))
        .expect("ES registers");
    runtime
        .register(definition(
            "nq",
            vec![market(series("NQ"), StreamRequirements::BARS)],
            1,
            StudyInvalidationPolicy::SameRange,
        ))
        .expect("NQ registers");

    assert!(matches!(
        runtime.reconcile_market_leases(&mut engine),
        Err(StudyRuntimeError::MarketDemand(
            EngineError::DataLeaseLimitExceeded { .. }
        ))
    ));
    assert_eq!(engine.data_lease_count(), 0);
    assert_eq!(
        engine.subscriptions(),
        [] as [(BarSeriesKey, aeris_market_engine::SubscriptionStatus); 0]
    );
}

#[test]
fn native_market_input_is_a_zero_copy_fixed_point_view_of_canonical_state() {
    let mut runtime = StudyRuntime::new(config(4));
    let mut engine = engine(2);
    let source = series("ES");
    let study = runtime
        .register(definition(
            "native_input",
            vec![market(source.clone(), StreamRequirements::BARS)],
            1,
            StudyInvalidationPolicy::SameRange,
        ))
        .expect("study registers");
    runtime
        .reconcile_market_leases(&mut engine)
        .expect("study lease installs");
    assert!(
        runtime
            .market_input(&engine, study, 0)
            .expect("dependency is valid")
            .is_none(),
        "valid studies must observe loading instead of fabricated rows"
    );

    engine
        .install_history(ProviderGeneration(NonZeroU64::MIN), &source, 2, 3, bars())
        .expect("canonical history installs");
    let canonical = engine.series_snapshot(&source).expect("canonical snapshot");
    let input = runtime
        .market_input(&engine, study, 0)
        .expect("dependency is valid")
        .expect("canonical data is ready");
    assert_eq!(input.series(), &source);
    assert!(!input.forming());
    assert_eq!(
        input.publication_generation(),
        canonical.publication_generation
    );
    assert!(std::ptr::eq(input.bars().as_ptr(), canonical.bars.as_ptr()));

    let close = input.field(StudyBarField::Close);
    assert_eq!(close.len(), 2);
    assert_eq!(close.scale(), 2);
    assert_eq!(close.value(0), Some(10_500));
    assert_eq!(
        close.exchange_timestamp_unix_nanos(1),
        Some(1_700_000_060_000_000_000)
    );
    let volume = input.field(StudyBarField::Volume);
    assert_eq!(volume.scale(), 3);
    assert_eq!(volume.value(1), Some(1_500));
}

#[test]
fn native_market_input_rejects_output_dependencies_without_guessing_alignment() {
    let mut runtime = StudyRuntime::new(config(4));
    let engine = engine(2);
    let producer = runtime
        .register(definition(
            "producer",
            vec![market(series("ES"), StreamRequirements::BARS)],
            1,
            StudyInvalidationPolicy::SameRange,
        ))
        .expect("producer registers");
    let consumer = runtime
        .register(definition(
            "consumer",
            vec![StudyDependency::Output(StudyOutputId {
                study_id: producer,
                output_index: 0,
            })],
            1,
            StudyInvalidationPolicy::SameRange,
        ))
        .expect("consumer registers");

    assert_eq!(
        runtime
            .market_input(&engine, consumer, 0)
            .expect_err("output dependency is not market data"),
        StudyRuntimeError::DependencyIsNotMarket {
            study_id: consumer,
            dependency_index: 0,
        }
    );
}

#[test]
fn production_studies_cannot_depend_on_another_consumers_outputs() {
    let mut runtime = StudyRuntime::new(config(4));
    let first_owner = ConsumerId(NonZeroU64::new(1).expect("owner"));
    let second_owner = ConsumerId(NonZeroU64::new(2).expect("owner"));
    let producer = runtime
        .register_for_consumer(
            first_owner,
            definition(
                "producer",
                vec![market(series("ES"), StreamRequirements::BARS)],
                1,
                StudyInvalidationPolicy::SameRange,
            ),
        )
        .expect("producer registers");
    let result = runtime.register_for_consumer(
        second_owner,
        definition(
            "consumer",
            vec![StudyDependency::Output(StudyOutputId {
                study_id: producer,
                output_index: 0,
            })],
            1,
            StudyInvalidationPolicy::SameRange,
        ),
    );

    assert!(matches!(
        result,
        Err(StudyRuntimeError::CrossConsumerDependency { dependency, .. })
            if dependency.study_id == producer
    ));
    assert_eq!(runtime.owner(producer), Some(first_owner));
    assert_eq!(runtime.len(), 1);
    assert_eq!(runtime.remove_consumer(first_owner), vec![producer]);
    assert!(runtime.is_empty());
}

#[test]
fn dirty_ranges_propagate_in_dependency_order() {
    let mut runtime = StudyRuntime::new(config(8));
    let source = series("ES");
    let price = runtime
        .register(definition(
            "price_transform",
            vec![market(source.clone(), StreamRequirements::BARS)],
            1,
            StudyInvalidationPolicy::SameRange,
        ))
        .expect("source study registers");
    let rolling = runtime
        .register(definition(
            "rolling",
            vec![StudyDependency::Output(StudyOutputId {
                study_id: price,
                output_index: 0,
            })],
            1,
            StudyInvalidationPolicy::TrailingWindow { bars: bound(3) },
        ))
        .expect("rolling study registers");
    let recursive = runtime
        .register(definition(
            "recursive",
            vec![StudyDependency::Output(StudyOutputId {
                study_id: rolling,
                output_index: 0,
            })],
            1,
            StudyInvalidationPolicy::FromFirstChanged,
        ))
        .expect("recursive study registers");

    let plan = runtime
        .plan_market_change(
            &source,
            StudyDirtyRange::bounded(10, 11).expect("dirty range"),
        )
        .expect("plan");
    assert_eq!(
        plan,
        vec![
            StudyCalculation {
                study_id: price,
                range: StudyDirtyRange::bounded(10, 11).expect("range"),
            },
            StudyCalculation {
                study_id: rolling,
                range: StudyDirtyRange::bounded(10, 13).expect("range"),
            },
            StudyCalculation {
                study_id: recursive,
                range: StudyDirtyRange::to_tail(10),
            },
        ]
    );
}

#[test]
fn output_changes_only_schedule_actual_downstream_dependencies() {
    let mut runtime = StudyRuntime::new(config(8));
    let source = series("ES");
    let producer = runtime
        .register(definition(
            "two_outputs",
            vec![market(source, StreamRequirements::BARS)],
            2,
            StudyInvalidationPolicy::SameRange,
        ))
        .expect("producer registers");
    let first = runtime
        .register(definition(
            "first_consumer",
            vec![StudyDependency::Output(StudyOutputId {
                study_id: producer,
                output_index: 0,
            })],
            1,
            StudyInvalidationPolicy::SameRange,
        ))
        .expect("first consumer registers");
    let second = runtime
        .register(definition(
            "second_consumer",
            vec![StudyDependency::Output(StudyOutputId {
                study_id: producer,
                output_index: 1,
            })],
            1,
            StudyInvalidationPolicy::SameRange,
        ))
        .expect("second consumer registers");

    let plan = runtime
        .plan_output_change(
            StudyOutputId {
                study_id: producer,
                output_index: 0,
            },
            StudyDirtyRange::bounded(4, 5).expect("range"),
        )
        .expect("downstream plan");
    assert_eq!(plan.len(), 1);
    assert_eq!(plan[0].study_id, first);
    assert_ne!(plan[0].study_id, second);
}

#[test]
fn forward_and_missing_output_dependencies_are_rejected() {
    let mut runtime = StudyRuntime::new(config(8));
    let unknown = StudyInstanceId(NonZeroU64::new(99).expect("non-zero"));
    let error = runtime
        .register(definition(
            "invalid",
            vec![StudyDependency::Output(StudyOutputId {
                study_id: unknown,
                output_index: 0,
            })],
            1,
            StudyInvalidationPolicy::SameRange,
        ))
        .expect_err("forward dependency is rejected");
    assert_eq!(
        error,
        StudyRuntimeError::UnknownOutput(StudyOutputId {
            study_id: unknown,
            output_index: 0,
        })
    );

    let producer = runtime
        .register(definition(
            "producer",
            vec![market(series("ES"), StreamRequirements::BARS)],
            1,
            StudyInvalidationPolicy::SameRange,
        ))
        .expect("producer registers");
    assert_eq!(
        runtime
            .register(definition(
                "bad_output",
                vec![StudyDependency::Output(StudyOutputId {
                    study_id: producer,
                    output_index: 1,
                })],
                1,
                StudyInvalidationPolicy::SameRange,
            ))
            .expect_err("missing output is rejected"),
        StudyRuntimeError::UnknownOutput(StudyOutputId {
            study_id: producer,
            output_index: 1,
        })
    );
}

#[test]
fn removing_a_source_removes_its_downstream_subtree_and_releases_demand() {
    let mut runtime = StudyRuntime::new(config(8));
    let es = series("ES");
    let nq = series("NQ");
    let source = runtime
        .register(definition(
            "source",
            vec![market(es.clone(), StreamRequirements::BARS)],
            1,
            StudyInvalidationPolicy::SameRange,
        ))
        .expect("source registers");
    let dependent = runtime
        .register(definition(
            "dependent",
            vec![StudyDependency::Output(StudyOutputId {
                study_id: source,
                output_index: 0,
            })],
            1,
            StudyInvalidationPolicy::SameRange,
        ))
        .expect("dependent registers");
    let independent = runtime
        .register(definition(
            "independent",
            vec![market(nq.clone(), StreamRequirements::BARS)],
            1,
            StudyInvalidationPolicy::SameRange,
        ))
        .expect("independent registers");

    let removed = runtime.remove_subtree(source).expect("source is live");
    assert_eq!(removed, vec![source, dependent]);
    assert_eq!(runtime.len(), 1);
    assert!(runtime.definition(independent).is_some());
    let requirements = runtime.market_requirements();
    assert!(!requirements.contains_key(&es));
    assert!(requirements.contains_key(&nq));
}

#[test]
fn runtime_enforces_instance_dependency_and_output_bounds() {
    let mut runtime = StudyRuntime::new(StudyRuntimeConfig {
        maximum_studies: bound(1),
        maximum_dependencies_per_study: bound(1),
        maximum_outputs_per_study: bound(1),
        maximum_points_per_output: bound(64),
        maximum_total_output_points: bound(64),
        maximum_state_bytes_per_study: bound(64),
        maximum_total_state_bytes: bound(64),
    });
    runtime
        .register(definition(
            "one",
            vec![market(series("ES"), StreamRequirements::BARS)],
            1,
            StudyInvalidationPolicy::SameRange,
        ))
        .expect("first study fits");
    assert_eq!(
        runtime
            .register(definition(
                "two",
                vec![market(series("NQ"), StreamRequirements::BARS)],
                1,
                StudyInvalidationPolicy::SameRange,
            ))
            .expect_err("instance bound"),
        StudyRuntimeError::StudyLimitExceeded { maximum: 1 }
    );

    let runtime = StudyRuntime::new(StudyRuntimeConfig {
        maximum_studies: bound(4),
        maximum_dependencies_per_study: bound(1),
        maximum_outputs_per_study: bound(1),
        maximum_points_per_output: bound(64),
        maximum_total_output_points: bound(64),
        maximum_state_bytes_per_study: bound(64),
        maximum_total_state_bytes: bound(64),
    });
    assert_eq!(
        runtime
            .validate_definition(
                None,
                &definition(
                    "too_many_dependencies",
                    vec![
                        market(series("ES"), StreamRequirements::BARS),
                        market(series("NQ"), StreamRequirements::BARS),
                    ],
                    1,
                    StudyInvalidationPolicy::SameRange,
                )
            )
            .expect_err("dependency bound"),
        StudyRuntimeError::TooManyDependencies { maximum: 1 }
    );
    assert_eq!(
        runtime
            .validate_definition(
                None,
                &definition(
                    "too_many_outputs",
                    vec![market(series("ES"), StreamRequirements::BARS)],
                    2,
                    StudyInvalidationPolicy::SameRange,
                )
            )
            .expect_err("output bound"),
        StudyRuntimeError::TooManyOutputs { maximum: 1 }
    );
}

#[test]
fn output_presentation_metadata_is_bounded_nonempty_and_uniquely_identified() {
    let runtime = StudyRuntime::new(config(4));
    let mut missing = definition(
        "missing_output",
        vec![market(series("ES"), StreamRequirements::BARS)],
        1,
        StudyInvalidationPolicy::SameRange,
    );
    missing.outputs.clear();
    assert_eq!(
        runtime
            .validate_definition(None, &missing)
            .expect_err("at least one output is required"),
        StudyRuntimeError::MissingOutput
    );

    let mut duplicate = definition(
        "duplicate_output",
        vec![market(series("ES"), StreamRequirements::BARS)],
        2,
        StudyInvalidationPolicy::SameRange,
    );
    duplicate.outputs[1].identifier = duplicate.outputs[0].identifier.clone();
    assert_eq!(
        runtime
            .validate_definition(None, &duplicate)
            .expect_err("output identifiers are stable unique keys"),
        StudyRuntimeError::DuplicateOutputIdentifier
    );

    let mut empty_title = definition(
        "empty_title",
        vec![market(series("ES"), StreamRequirements::BARS)],
        1,
        StudyInvalidationPolicy::SameRange,
    );
    empty_title.outputs[0].title.clear();
    assert_eq!(
        runtime
            .validate_definition(None, &empty_title)
            .expect_err("visible output title is required"),
        StudyRuntimeError::InvalidOutputTitle
    );

    let mut empty_legend_label = definition(
        "empty_legend_label",
        vec![market(series("ES"), StreamRequirements::BARS)],
        1,
        StudyInvalidationPolicy::SameRange,
    );
    empty_legend_label.outputs[0].legend_label = Some("   ".to_string());
    assert_eq!(
        runtime
            .validate_definition(None, &empty_legend_label)
            .expect_err("visible legend labels cannot be empty"),
        StudyRuntimeError::InvalidOutputTitle
    );

    let mut long_legend_label = definition(
        "long_legend_label",
        vec![market(series("ES"), StreamRequirements::BARS)],
        1,
        StudyInvalidationPolicy::SameRange,
    );
    long_legend_label.outputs[0].legend_label =
        Some("x".repeat(MAXIMUM_STUDY_IDENTIFIER_BYTES + 1));
    assert_eq!(
        runtime
            .validate_definition(None, &long_legend_label)
            .expect_err("legend labels share the bounded output metadata budget"),
        StudyRuntimeError::OutputMetadataTooLong {
            maximum: MAXIMUM_STUDY_IDENTIFIER_BYTES,
        }
    );
}

#[test]
fn output_presentation_semantics_are_validated_before_registration() {
    let runtime = StudyRuntime::new(config(4));

    let mut reversed_threshold = definition(
        "reversed_threshold",
        vec![market(series("ES"), StreamRequirements::BARS)],
        1,
        StudyInvalidationPolicy::SameRange,
    );
    reversed_threshold.outputs[0].threshold_region = Some(StudyThresholdRegion {
        lower: StudyDecimal {
            mantissa: 70,
            scale: 0,
        },
        upper: StudyDecimal {
            mantissa: 30,
            scale: 0,
        },
    });
    assert_eq!(
        runtime
            .validate_definition(None, &reversed_threshold)
            .expect_err("threshold regions must be strictly ordered"),
        StudyRuntimeError::InvalidOutputPresentation
    );

    let mut threshold_histogram = definition(
        "threshold_histogram",
        vec![market(series("ES"), StreamRequirements::BARS)],
        1,
        StudyInvalidationPolicy::SameRange,
    );
    threshold_histogram.outputs[0].plot = StudyPlotKind::Histogram;
    threshold_histogram.outputs[0].threshold_region = Some(StudyThresholdRegion {
        lower: StudyDecimal {
            mantissa: 20,
            scale: 0,
        },
        upper: StudyDecimal {
            mantissa: 80,
            scale: 0,
        },
    });
    assert_eq!(
        runtime
            .validate_definition(None, &threshold_histogram)
            .expect_err("threshold region is a line/area presentation"),
        StudyRuntimeError::InvalidOutputPresentation
    );

    let mut momentum_line = definition(
        "momentum_line",
        vec![market(series("ES"), StreamRequirements::BARS)],
        1,
        StudyInvalidationPolicy::SameRange,
    );
    momentum_line.outputs[0].point_style = StudyPointStyle::MomentumHistogram;
    assert_eq!(
        runtime
            .validate_definition(None, &momentum_line)
            .expect_err("momentum point style is histogram-only"),
        StudyRuntimeError::InvalidOutputPresentation
    );

    let mut valid = definition(
        "valid_richer_output",
        vec![market(series("ES"), StreamRequirements::BARS)],
        1,
        StudyInvalidationPolicy::SameRange,
    );
    valid.outputs[0].threshold_region = Some(StudyThresholdRegion {
        lower: StudyDecimal {
            mantissa: 30,
            scale: 0,
        },
        upper: StudyDecimal {
            mantissa: 70,
            scale: 0,
        },
    });
    runtime
        .validate_definition(None, &valid)
        .expect("valid threshold presentation is accepted");
}
