use tradingplot_study_sdk::{
    BarPeriod, BarSeriesKey, NativeStudyProgram, NativeStudyRegistration, StreamRequirements,
    StudyBarField, StudyDefinition, StudyDependency, StudyExecutionContext, StudyInputSeries,
    StudyInvalidationPolicy, StudyMarketInput, StudyOutputSpec, StudyPaneTarget, StudyPlotKind,
    StudyPointStyle, StudyScaleTarget, StudySettings, fixed_point_to_f64,
};

fn source() -> BarSeriesKey {
    BarSeriesKey {
        provider_id: "provider".to_string(),
        instrument_id: "instrument".to_string(),
        entitlement_id: "entitlement".to_string(),
        period: BarPeriod::time(60).expect("valid one-minute period"),
        definition_version: 1,
    }
}

fn calculate(context: &mut StudyExecutionContext<'_>) -> Result<(), String> {
    let (inputs, outputs) = context.split();
    let StudyInputSeries::Market(series) = inputs
        .input(0)
        .ok_or_else(|| "market input is unavailable".to_string())?
    else {
        return Err("expected a market input".to_string());
    };
    let close = series.field(StudyBarField::Close);
    let dirty = inputs.dirty_range();
    let output = outputs
        .first_mut()
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
        identifier: "example.stateless_close".to_string(),
        dependencies: vec![StudyDependency::Market(StudyMarketInput {
            series: source(),
            streams: StreamRequirements::BARS,
        })],
        settings: Vec::new(),
        outputs: vec![StudyOutputSpec {
            identifier: "close".to_string(),
            title: "Stateless Close".to_string(),
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
        program: NativeStudyProgram::stateless(calculate),
    }
}

fn main() {
    let registration = registration();
    assert_eq!(
        registration.definition.identifier,
        "example.stateless_close"
    );
}
