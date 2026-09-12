use axiusflow_study_sdk::{
    BarPeriod, BarSeriesKey, NativeStudyProgram, NativeStudyRegistration, StreamRequirements,
    StudyBarField, StudyDefinition, StudyDependency, StudyExecutionContext, StudyInputSeries,
    StudyInvalidationPolicy, StudyMarketInput, StudyOutputSpec, StudyPaneTarget, StudyPlotKind,
    StudyPointStyle, StudyScaleTarget, StudySettings, fixed_point_to_f64,
};

fn series(seconds: u32) -> BarSeriesKey {
    BarSeriesKey {
        provider_id: "provider".to_string(),
        instrument_id: "instrument".to_string(),
        entitlement_id: "entitlement".to_string(),
        period: BarPeriod::time(seconds).expect("valid time period"),
        definition_version: 1,
    }
}

fn calculate(context: &mut StudyExecutionContext<'_>) -> Result<(), String> {
    let (inputs, outputs) = context.split();
    let StudyInputSeries::Market(primary) = inputs
        .input(0)
        .ok_or_else(|| "primary input is unavailable".to_string())?
    else {
        return Err("expected primary market input".to_string());
    };
    let StudyInputSeries::Market(secondary) = inputs
        .input(1)
        .ok_or_else(|| "secondary input is unavailable".to_string())?
    else {
        return Err("expected secondary market input".to_string());
    };
    let primary_close = primary.field(StudyBarField::Close);
    let secondary_close = secondary.field(StudyBarField::Close);
    let output = outputs
        .first_mut()
        .ok_or_else(|| "output is unavailable".to_string())?;
    let dirty = inputs.dirty_range();
    let end = dirty
        .end_exclusive
        .unwrap_or(output.len())
        .min(output.len());
    for index in dirty.start..end {
        let Some(timestamp) = primary_close.exchange_timestamp_unix_nanos(index) else {
            output.set(index, None)?;
            continue;
        };
        let mut left = 0;
        let mut right = secondary_close.len();
        while left < right {
            let middle = left + (right - left) / 2;
            let candidate = secondary_close
                .exchange_timestamp_unix_nanos(middle)
                .ok_or_else(|| "secondary timestamp is unavailable".to_string())?;
            if candidate <= timestamp {
                left = middle + 1;
            } else {
                right = middle;
            }
        }
        let value = left
            .checked_sub(1)
            .and_then(|secondary_index| secondary_close.value(secondary_index))
            .map(|value| {
                fixed_point_to_f64(value, secondary_close.scale())
                    .ok_or_else(|| "fixed-point value cannot be represented".to_string())
            })
            .transpose()?;
        output.set(index, value)?;
    }
    Ok(())
}

fn registration() -> NativeStudyRegistration {
    let definition = StudyDefinition {
        identifier: "example.multi_timeframe_close".to_string(),
        dependencies: vec![
            StudyDependency::Market(StudyMarketInput {
                series: series(60),
                streams: StreamRequirements::BARS,
            }),
            StudyDependency::Market(StudyMarketInput {
                series: series(300),
                streams: StreamRequirements::BARS,
            }),
        ],
        settings: Vec::new(),
        outputs: vec![StudyOutputSpec {
            identifier: "five_minute_close".to_string(),
            title: "5m Close on 1m".to_string(),
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
    assert_eq!(registration.definition.dependencies.len(), 2);
}
