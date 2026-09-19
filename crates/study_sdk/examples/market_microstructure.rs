use tradingplot_study_sdk::{
    BarPeriod, BarSeriesKey, MarketStream, NativeStudyProgram, NativeStudyRegistration,
    StreamRequirements, StudyDefinition, StudyDependency, StudyExecutionContext,
    StudyInvalidationPolicy, StudyMarketInput, StudyOutputSpec, StudyPaneTarget, StudyPlotKind,
    StudyPointStyle, StudyScaleTarget, StudySettings, fixed_point_to_f64,
};

fn calculate(context: &mut StudyExecutionContext<'_>) -> Result<(), String> {
    let (inputs, outputs) = context.split();
    let live = inputs.live_market(0);
    let output = outputs
        .first_mut()
        .ok_or_else(|| "output is unavailable".to_string())?;
    let dirty = inputs.dirty_range();
    let end = dirty
        .end_exclusive
        .unwrap_or(output.len())
        .min(output.len());
    if dirty.start >= end {
        return Ok(());
    }

    let score = live.and_then(|live| {
        let quote = live.quote()?;
        let bid = quote.bid()?;
        let ask = quote.ask()?;
        let bid_price = fixed_point_to_f64(bid.price, quote.price_scale())?;
        let ask_price = fixed_point_to_f64(ask.price, quote.price_scale())?;
        let midpoint = bid_price.midpoint(ask_price);

        let depth_imbalance = live.depth().and_then(|depth| {
            let bid_quantity =
                fixed_point_to_f64(depth.bids().next()?.quantity, depth.quantity_scale())?;
            let ask_quantity =
                fixed_point_to_f64(depth.asks().next()?.quantity, depth.quantity_scale())?;
            let total = bid_quantity + ask_quantity;
            (total > 0.0).then_some((bid_quantity - ask_quantity) / total)
        });
        let recent_trade_count = live
            .trades()
            .and_then(|trades| u32::try_from(trades.len()).ok())
            .map_or(0.0, f64::from);
        Some(midpoint + depth_imbalance.unwrap_or(0.0) + recent_trade_count.min(1.0e-6))
    });

    // Current quote/trade/depth are point-in-time state, so this bar-aligned example writes only
    // the newest row in the dirty range. Pure non-bar output timelines are intentionally separate.
    output.set(end - 1, score)?;
    Ok(())
}

fn registration() -> NativeStudyRegistration {
    let streams = StreamRequirements::BARS
        .with(MarketStream::Quotes)
        .with(MarketStream::Trades)
        .with(MarketStream::Depth);
    let definition = StudyDefinition {
        identifier: "example.microstructure".to_string(),
        dependencies: vec![StudyDependency::Market(StudyMarketInput {
            series: BarSeriesKey {
                provider_id: "provider".to_string(),
                instrument_id: "instrument".to_string(),
                entitlement_id: "entitlement".to_string(),
                period: BarPeriod::time(60).expect("valid one-minute period"),
                definition_version: 1,
            },
            streams,
        })],
        settings: Vec::new(),
        outputs: vec![StudyOutputSpec {
            identifier: "score".to_string(),
            title: "Microstructure Score".to_string(),
            legend_label: None,
            plot: StudyPlotKind::Line,
            pane: StudyPaneTarget::Dedicated { group: 0 },
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
    let StudyDependency::Market(input) = &registration.definition.dependencies[0] else {
        unreachable!("example declares one market dependency");
    };
    assert!(input.streams.contains(MarketStream::Depth));
}
