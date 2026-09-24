use aeris_study_sdk::{
    BarPeriod, BarSeriesKey, NativeStudyProgram, NativeStudyRegistration, NativeStudyState,
    StreamRequirements, StudyBarField, StudyDefinition, StudyDependency, StudyExecutionContext,
    StudyInputSeries, StudyInvalidationPolicy, StudyMarketInput, StudyOutputSpec, StudyPaneTarget,
    StudyPlotKind, StudyPointStyle, StudyScaleTarget, StudySettings, fixed_point_to_f64,
};

#[derive(Clone, Default)]
struct ExecutionState {
    invocations: u64,
}

fn state_bytes(_state: &ExecutionState) -> usize {
    std::mem::size_of::<ExecutionState>()
}

fn clone_state(state: &ExecutionState) -> ExecutionState {
    state.clone()
}

fn create_state(settings: &StudySettings) -> Result<NativeStudyState, String> {
    if settings.get("unexpected").is_some() {
        return Err("unexpected state setting".to_string());
    }
    Ok(NativeStudyState::new_transactional(
        ExecutionState::default(),
        state_bytes,
        clone_state,
    ))
}

fn calculate(context: &mut StudyExecutionContext<'_>) -> Result<(), String> {
    let series = match context
        .input(0)
        .ok_or_else(|| "market input is unavailable".to_string())?
    {
        StudyInputSeries::Market(series) => series.clone(),
        StudyInputSeries::Output(_) => return Err("expected a market input".to_string()),
    };
    context
        .state_mut::<ExecutionState>()
        .ok_or_else(|| "execution state is unavailable".to_string())?
        .invocations += 1;

    let close = series.field(StudyBarField::Close);
    let dirty = context.dirty_range();
    let output = context
        .output(0)
        .ok_or_else(|| "output is unavailable".to_string())?;
    let end = dirty
        .end_exclusive
        .unwrap_or(output.len())
        .min(output.len());
    for index in dirty.start..end {
        let value = close
            .value(index)
            .map(|value| {
                fixed_point_to_f64(value, close.scale())
                    .ok_or_else(|| "fixed-point value cannot be represented".to_string())
            })
            .transpose()?;
        output.set(index, value)?;
    }
    Ok(())
}

fn registration() -> NativeStudyRegistration {
    let definition = StudyDefinition {
        identifier: "example.stateful_close".to_string(),
        dependencies: vec![StudyDependency::Market(StudyMarketInput {
            series: BarSeriesKey {
                provider_id: "provider".to_string(),
                instrument_id: "instrument".to_string(),
                entitlement_id: "entitlement".to_string(),
                period: BarPeriod::time(60).expect("valid one-minute period"),
                definition_version: 1,
            },
            streams: StreamRequirements::BARS,
        })],
        settings: Vec::new(),
        outputs: vec![StudyOutputSpec {
            identifier: "close".to_string(),
            title: "Stateful Close".to_string(),
            legend_label: None,
            plot: StudyPlotKind::Line,
            pane: StudyPaneTarget::Price,
            scale: StudyScaleTarget::Primary,
            threshold_region: None,
            point_style: StudyPointStyle::Uniform,
        }],
        invalidation: StudyInvalidationPolicy::SameRange,
    };
    NativeStudyRegistration {
        settings: StudySettings::defaults(&definition.settings).expect("valid default settings"),
        definition,
        program: NativeStudyProgram::stateful(calculate, create_state),
    }
}

fn main() {
    let registration = registration();
    assert!(registration.program.state_factory.is_some());
}
