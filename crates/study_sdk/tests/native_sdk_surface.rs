use axiusflow_study_sdk::{
    BarPeriod, BarSeriesKey, NativeStudyProgram, NativeStudyRegistration, NativeStudyState,
    StreamRequirements, StudyDefinition, StudyDependency, StudyExecutionContext, StudyInstanceId,
    StudyInvalidationPolicy, StudyMarketInput, StudyOutputSpec, StudyPaneTarget, StudyPlotKind,
    StudyPointStyle, StudyScaleTarget, StudySettings,
};

#[derive(Clone)]
struct CounterState {
    executions: u32,
}

fn counter_state_bytes(_state: &CounterState) -> usize {
    size_of::<CounterState>()
}

fn counter_state(settings: &StudySettings) -> Result<NativeStudyState, String> {
    if !settings.is_empty() {
        return Err("counter study declares no settings".to_string());
    }
    Ok(NativeStudyState::new(
        CounterState { executions: 0 },
        counter_state_bytes,
    ))
}

fn calculate_counter(context: &mut StudyExecutionContext<'_>) -> Result<(), String> {
    let (inputs, state, outputs) = context
        .split_with_state::<CounterState>()
        .ok_or_else(|| "counter state is unavailable".to_string())?;
    state.executions = state.executions.saturating_add(1);
    let output = outputs
        .first_mut()
        .ok_or_else(|| "counter output is unavailable".to_string())?;
    let dirty = inputs.dirty_range();
    let end = dirty
        .end_exclusive
        .unwrap_or(output.len())
        .min(output.len());
    for row in dirty.start..end {
        output.set(row, Some(f64::from(state.executions)))?;
    }
    Ok(())
}

fn calculate_microstructure(context: &mut StudyExecutionContext<'_>) -> Result<(), String> {
    let (inputs, outputs) = context.split();
    let live = inputs
        .live_market(0)
        .ok_or_else(|| "live market input is unavailable".to_string())?;
    let quote = live
        .quote()
        .ok_or_else(|| "quote is not available yet".to_string())?;
    let trades = live
        .trades()
        .ok_or_else(|| "trade window is unavailable".to_string())?;
    let depth = live
        .depth()
        .ok_or_else(|| "depth is unavailable".to_string())?;
    let best_bid = quote.bid().map(|level| level.price);
    let best_ask = depth.asks().next().map(|level| level.price);
    let _latest_trade = trades.get(trades.len().saturating_sub(1));
    let output = outputs
        .first_mut()
        .ok_or_else(|| "microstructure output is unavailable".to_string())?;
    let dirty = inputs.dirty_range();
    let end = dirty
        .end_exclusive
        .unwrap_or(output.len())
        .min(output.len());
    for row in dirty.start..end {
        let spread = best_bid
            .zip(best_ask)
            .map(|(bid, ask)| i32::try_from(ask - bid))
            .transpose()
            .map_err(|_| "spread is out of example range".to_string())?
            .map(f64::from);
        output.set(row, spread)?;
    }
    Ok(())
}

#[test]
fn a_stateful_native_study_can_be_defined_through_the_sdk_facade_only() {
    let definition = StudyDefinition {
        identifier: "example.counter".to_string(),
        dependencies: vec![StudyDependency::Market(StudyMarketInput {
            series: BarSeriesKey {
                provider_id: "provider".to_string(),
                instrument_id: "instrument".to_string(),
                entitlement_id: "entitlement".to_string(),
                period: BarPeriod::time(60).expect("minute period"),
                definition_version: 1,
            },
            streams: StreamRequirements::BARS,
        })],
        settings: Vec::new(),
        outputs: vec![StudyOutputSpec {
            identifier: "count".to_string(),
            title: "Counter".to_string(),
            legend_label: None,
            plot: StudyPlotKind::Line,
            pane: StudyPaneTarget::Price,
            scale: StudyScaleTarget::Primary,
            threshold_region: None,
            point_style: StudyPointStyle::default(),
        }],
        invalidation: StudyInvalidationPolicy::FromFirstChanged,
    };
    let registration = NativeStudyRegistration {
        settings: StudySettings::defaults(&definition.settings).expect("valid settings"),
        definition,
        program: NativeStudyProgram::stateful(calculate_counter, counter_state),
    };

    assert_eq!(registration.definition.identifier, "example.counter");
    assert!(registration.program.state_factory.is_some());
}

#[test]
fn indicator_on_indicator_dependency_is_expressible_through_the_sdk_facade_only() {
    let upstream = StudyInstanceId::try_from_u64(7).expect("non-zero study id");
    let dependency = StudyDependency::Output(upstream.output(2));

    assert!(matches!(
        dependency,
        StudyDependency::Output(output)
            if output.study_id == upstream && output.output_index == 2
    ));
}

#[test]
fn quote_trade_and_depth_inputs_are_expressible_through_the_sdk_facade_only() {
    let streams = StreamRequirements::BARS
        .with(axiusflow_study_sdk::MarketStream::Trades)
        .with(axiusflow_study_sdk::MarketStream::Quotes)
        .with(axiusflow_study_sdk::MarketStream::Depth);
    let definition = StudyDefinition {
        identifier: "example.microstructure".to_string(),
        dependencies: vec![StudyDependency::Market(StudyMarketInput {
            series: BarSeriesKey {
                provider_id: "provider".to_string(),
                instrument_id: "instrument".to_string(),
                entitlement_id: "entitlement".to_string(),
                period: BarPeriod::time(60).expect("minute period"),
                definition_version: 1,
            },
            streams,
        })],
        settings: Vec::new(),
        outputs: vec![StudyOutputSpec {
            identifier: "spread".to_string(),
            title: "Spread".to_string(),
            legend_label: None,
            plot: StudyPlotKind::Line,
            pane: StudyPaneTarget::Price,
            scale: StudyScaleTarget::Primary,
            threshold_region: None,
            point_style: StudyPointStyle::default(),
        }],
        invalidation: StudyInvalidationPolicy::FromFirstChanged,
    };
    let registration = NativeStudyRegistration {
        settings: StudySettings::defaults(&definition.settings).expect("valid settings"),
        definition,
        program: NativeStudyProgram::stateless(calculate_microstructure),
    };

    assert_eq!(registration.definition.identifier, "example.microstructure");
}
