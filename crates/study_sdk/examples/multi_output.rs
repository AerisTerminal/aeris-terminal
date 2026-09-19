use tradingplot_study_sdk::{
    BarPeriod, BarSeriesKey, NativeStudyProgram, NativeStudyRegistration, StreamRequirements,
    StudyBarField, StudyDefinition, StudyDependency, StudyExecutionContext, StudyInputSeries,
    StudyInvalidationPolicy, StudyMarketInput, StudyOutputSpec, StudyPaneTarget, StudyPlotKind,
    StudyPointStyle, StudyScaleTarget, StudySettings, fixed_point_to_f64,
};

fn calculate(context: &mut StudyExecutionContext<'_>) -> Result<(), String> {
    let (inputs, outputs) = context.split();
    let StudyInputSeries::Market(series) = inputs
        .input(0)
        .ok_or_else(|| "market input is unavailable".to_string())?
    else {
        return Err("expected a market input".to_string());
    };
    let high = series.field(StudyBarField::High);
    let low = series.field(StudyBarField::Low);
    let dirty = inputs.dirty_range();
    let [high_output, low_output] = outputs else {
        return Err("expected two outputs".to_string());
    };
    let end = dirty
        .end_exclusive
        .unwrap_or(high_output.len())
        .min(high_output.len());
    for index in dirty.start..end {
        let high_value = high
            .value(index)
            .map(|value| {
                fixed_point_to_f64(value, high.scale())
                    .ok_or_else(|| "fixed-point value cannot be represented".to_string())
            })
            .transpose()?;
        let low_value = low
            .value(index)
            .map(|value| {
                fixed_point_to_f64(value, low.scale())
                    .ok_or_else(|| "fixed-point value cannot be represented".to_string())
            })
            .transpose()?;
        high_output.set(index, high_value)?;
        low_output.set(index, low_value)?;
    }
    Ok(())
}

fn output(identifier: &str, label: &str) -> StudyOutputSpec {
    StudyOutputSpec {
        identifier: identifier.to_string(),
        title: "High / Low".to_string(),
        legend_label: Some(label.to_string()),
        plot: StudyPlotKind::Line,
        pane: StudyPaneTarget::Price,
        scale: StudyScaleTarget::Primary,
        threshold_region: None,
        point_style: StudyPointStyle::Uniform,
    }
}

fn registration() -> NativeStudyRegistration {
    let definition = StudyDefinition {
        identifier: "example.high_low".to_string(),
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
        outputs: vec![output("high", "High"), output("low", "Low")],
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
    assert_eq!(registration.definition.outputs.len(), 2);
}
