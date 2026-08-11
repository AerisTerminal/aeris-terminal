use axiusflow_coinbase_market_adapter::{
    CoinbaseBarAggregator, CoinbaseBarAggregatorConfig, CoinbaseDecoder,
};
use axiusflow_observability::{
    DiagnosticsBenchmarkArm, DiagnosticsBenchmarkContext, DiagnosticsOverheadEvidence,
    FeedConnectionState, FeedCounter, FeedDiagnostics, FeedIdentity, LatencyBoundary,
    LatencyTimestampChain, LocalLatencyMetric,
};
use std::{
    env,
    fmt::Write as _,
    fs::{self, File},
    hint::black_box,
    io::{self, Write},
    num::{NonZeroU64, NonZeroUsize},
    path::{Path, PathBuf},
    time::Instant,
};

const DEFAULT_WARM_UP_SAMPLES: usize = 256;
const DEFAULT_MEASURED_SAMPLES: usize = 2_048;
const EVENTS_PER_SAMPLE: usize = 8_192;
const DETAILED_MAXIMUM_NANOS: u64 = 10_000_000_000;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let output = output_path()?;
    let context = DiagnosticsBenchmarkContext {
        hardware: hardware_context(),
        operating_system: operating_system_context(),
        workload: format!(
            "coinbase_decode_aggregate_with_feed_diagnostics_{EVENTS_PER_SAMPLE}_trades_per_sample"
        ),
        warm_up_samples: u64::try_from(DEFAULT_WARM_UP_SAMPLES)?,
        measured_samples: u64::try_from(DEFAULT_MEASURED_SAMPLES)?,
    };
    let message = benchmark_message()?;
    let (baseline, detailed) = measure(
        message.as_bytes(),
        DEFAULT_WARM_UP_SAMPLES,
        DEFAULT_MEASURED_SAMPLES,
    )?;
    let evidence = DiagnosticsOverheadEvidence {
        context,
        baseline,
        detailed,
    };
    let assessment = evidence.assess()?;
    write_evidence(&output, &evidence, assessment.passes)?;
    if !assessment.passes {
        return Err(format!(
            "diagnostics overhead exceeded budget: p99={} bp, p99.9={} bp",
            assessment.p99_regression_basis_points, assessment.p99_9_regression_basis_points
        )
        .into());
    }
    println!(
        "diagnostics overhead passed: p99={} bp, p99.9={} bp, evidence={}",
        assessment.p99_regression_basis_points,
        assessment.p99_9_regression_basis_points,
        output.display()
    );
    Ok(())
}

fn output_path() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let mut args = env::args_os().skip(1);
    match (args.next(), args.next()) {
        (Some(path), None) => Ok(PathBuf::from(path)),
        (None, None) => Ok(PathBuf::from(".cache/evidence/diagnostics_overhead.json")),
        _ => Err("usage: axiusflow_diagnostics_overhead [output-json]".into()),
    }
}

fn measure(
    message: &[u8],
    warm_up_samples: usize,
    measured_samples: usize,
) -> Result<(DiagnosticsBenchmarkArm, DiagnosticsBenchmarkArm), Box<dyn std::error::Error>> {
    let mut baseline_diagnostics = diagnostics(false)?;
    let mut detailed_diagnostics = diagnostics(true)?;
    let aggregator_config = CoinbaseBarAggregatorConfig::try_new(
        "BTC-USD",
        2,
        8,
        NonZeroUsize::new(1).unwrap_or(NonZeroUsize::MIN),
    )?;
    let total_samples = warm_up_samples.saturating_add(measured_samples);
    let mut baseline_durations = Vec::with_capacity(measured_samples);
    let mut detailed_durations = Vec::with_capacity(measured_samples);
    for sample_index in 0..total_samples {
        let (baseline_nanos, detailed_nanos) = if sample_index.is_multiple_of(2) {
            (
                measure_sample(
                    &mut baseline_diagnostics,
                    sample_index,
                    message,
                    &aggregator_config,
                )?,
                measure_sample(
                    &mut detailed_diagnostics,
                    sample_index,
                    message,
                    &aggregator_config,
                )?,
            )
        } else {
            let detailed_nanos = measure_sample(
                &mut detailed_diagnostics,
                sample_index,
                message,
                &aggregator_config,
            )?;
            let baseline_nanos = measure_sample(
                &mut baseline_diagnostics,
                sample_index,
                message,
                &aggregator_config,
            )?;
            (baseline_nanos, detailed_nanos)
        };
        if sample_index >= warm_up_samples {
            baseline_durations.push(baseline_nanos);
            detailed_durations.push(detailed_nanos);
        }
    }
    black_box(baseline_diagnostics);
    black_box(detailed_diagnostics);
    Ok((
        benchmark_arm(baseline_durations)?,
        benchmark_arm(detailed_durations)?,
    ))
}

fn diagnostics(detailed: bool) -> Result<FeedDiagnostics, Box<dyn std::error::Error>> {
    let mut diagnostics = FeedDiagnostics::new(
        FeedIdentity::try_new("coinbase", "advanced_trade_public", "production")?,
        detailed.then_some(NonZeroU64::new(DETAILED_MAXIMUM_NANOS).unwrap_or(NonZeroU64::MIN)),
    );
    diagnostics.begin_session(NonZeroU64::MIN, 0)?;
    diagnostics.set_connection_state(FeedConnectionState::Streaming);
    Ok(diagnostics)
}

fn measure_sample(
    diagnostics: &mut FeedDiagnostics,
    sample_index: usize,
    message: &[u8],
    aggregator_config: &CoinbaseBarAggregatorConfig,
) -> Result<u64, Box<dyn std::error::Error>> {
    let mut decoder = CoinbaseDecoder::new();
    let mut aggregator = CoinbaseBarAggregator::new(aggregator_config.clone());
    let started = Instant::now();
    let trades = decoder.decode(black_box(message))?;
    if trades.len() != EVENTS_PER_SAMPLE {
        return Err(format!(
            "benchmark decoded {} trades, expected {EVENTS_PER_SAMPLE}",
            trades.len()
        )
        .into());
    }
    for (event_index, trade) in trades.iter().enumerate() {
        black_box(aggregator.apply_trade_with_evidence(trade)?);
        record_diagnostics(diagnostics, sample_index, event_index)?;
    }
    let nanos = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
    black_box(decoder);
    black_box(aggregator);
    Ok(nanos / u64::try_from(EVENTS_PER_SAMPLE)?)
}

fn benchmark_arm(
    mut durations: Vec<u64>,
) -> Result<DiagnosticsBenchmarkArm, std::num::TryFromIntError> {
    durations.sort_unstable();
    Ok(DiagnosticsBenchmarkArm {
        p50_nanos: percentile(&durations, 500),
        p95_nanos: percentile(&durations, 950),
        p99_nanos: percentile(&durations, 990),
        p99_9_nanos: percentile(&durations, 999),
        maximum_nanos: *durations.last().unwrap_or(&0),
        sample_count: u64::try_from(durations.len())?,
        gaps: 0,
        overflows: 0,
        recoveries: 0,
    })
}

fn record_diagnostics(
    diagnostics: &mut FeedDiagnostics,
    sample_index: usize,
    event_index: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    let sequence = sample_index
        .saturating_mul(EVENTS_PER_SAMPLE)
        .saturating_add(event_index);
    let base = black_box(i64::try_from(sequence.saturating_mul(1_000))?);
    let mut chain = LatencyTimestampChain::new();
    chain.set(LatencyBoundary::SocketRead, base);
    chain.set(LatencyBoundary::Decode, base.saturating_add(90));
    chain.set(LatencyBoundary::CanonicalAccept, base.saturating_add(170));
    chain.set(LatencyBoundary::ModelPublish, base.saturating_add(260));
    chain.set(LatencyBoundary::UiEnqueue, base.saturating_add(330));
    chain.set(LatencyBoundary::FrameSubmit, base.saturating_add(420));
    chain.set(LatencyBoundary::FrameCallback, base.saturating_add(510));
    chain.set(LatencyBoundary::Present, base.saturating_add(600));
    for metric in LocalLatencyMetric::ALL {
        diagnostics.record_latency_chain(metric, &chain)?;
    }
    diagnostics.increment(FeedCounter::Trades);
    diagnostics.increment(FeedCounter::Publications);
    Ok(())
}

fn benchmark_message() -> Result<String, std::fmt::Error> {
    let mut message = String::with_capacity(EVENTS_PER_SAMPLE.saturating_mul(180));
    message.push_str(
        r#"{"channel":"market_trades","timestamp":"2023-02-09T20:19:35.39625135Z","sequence_num":1,"events":[{"type":"update","trades":["#,
    );
    for index in 0..EVENTS_PER_SAMPLE {
        if index > 0 {
            message.push(',');
        }
        write!(
            message,
            r#"{{"trade_id":"benchmark-{index}","product_id":"BTC-USD","price":"67001.25","size":"0.0042","side":"SELL","time":"2023-02-09T20:19:34.265Z"}}"#,
        )?;
    }
    message.push_str("]}]}");
    Ok(message)
}

fn percentile(sorted: &[u64], permille: usize) -> u64 {
    let rank = sorted.len().saturating_mul(permille).saturating_add(999) / 1_000;
    sorted[rank.saturating_sub(1).min(sorted.len().saturating_sub(1))]
}

fn hardware_context() -> String {
    let cpu = read_cpu_model().unwrap_or_else(|| "unknown-cpu".to_string());
    format!(
        "{cpu}; logical_cpus={}",
        std::thread::available_parallelism().map_or(1, NonZeroUsize::get)
    )
}

fn read_cpu_model() -> Option<String> {
    env::var("PROCESSOR_IDENTIFIER")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            fs::read_to_string("/proc/cpuinfo")
                .ok()?
                .lines()
                .find_map(|line| line.strip_prefix("model name"))
                .and_then(|line| {
                    line.split_once(':')
                        .map(|(_, value)| value.trim().to_string())
                })
                .filter(|value| !value.is_empty())
        })
}

fn operating_system_context() -> String {
    let pretty_name = fs::read_to_string("/etc/os-release")
        .ok()
        .and_then(|content| {
            content.lines().find_map(|line| {
                line.strip_prefix("PRETTY_NAME=")
                    .map(|value| value.trim_matches('"').to_string())
            })
        })
        .unwrap_or_else(|| env::consts::OS.to_string());
    format!("{pretty_name}; arch={}", env::consts::ARCH)
}

fn write_evidence(
    output: &Path,
    evidence: &DiagnosticsOverheadEvidence,
    passes: bool,
) -> io::Result<()> {
    if let Some(parent) = output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)?;
    }
    let mut file = File::create(output)?;
    writeln!(file, "{{")?;
    writeln!(file, "  \"schema_version\": 2,")?;
    writeln!(
        file,
        "  \"method\": \"paired_alternating_provider_workload_diagnostics_overhead\","
    )?;
    writeln!(file, "  \"passes\": {passes},")?;
    write_context(&mut file, &evidence.context)?;
    write_arm(&mut file, "baseline", evidence.baseline)?;
    writeln!(file, ",")?;
    write_arm(&mut file, "detailed", evidence.detailed)?;
    writeln!(file)?;
    writeln!(file, "}}")
}

fn write_context(file: &mut File, context: &DiagnosticsBenchmarkContext) -> io::Result<()> {
    writeln!(file, "  \"context\": {{")?;
    writeln!(
        file,
        "    \"hardware\": \"{}\",",
        json_escape(&context.hardware)
    )?;
    writeln!(
        file,
        "    \"operating_system\": \"{}\",",
        json_escape(&context.operating_system)
    )?;
    writeln!(
        file,
        "    \"workload\": \"{}\",",
        json_escape(&context.workload)
    )?;
    writeln!(
        file,
        "    \"warm_up_samples\": {},",
        context.warm_up_samples
    )?;
    writeln!(
        file,
        "    \"measured_samples\": {}",
        context.measured_samples
    )?;
    writeln!(file, "  }},")
}

fn write_arm(file: &mut File, name: &str, arm: DiagnosticsBenchmarkArm) -> io::Result<()> {
    writeln!(file, "  \"{name}\": {{")?;
    writeln!(file, "    \"p50_nanos\": {},", arm.p50_nanos)?;
    writeln!(file, "    \"p95_nanos\": {},", arm.p95_nanos)?;
    writeln!(file, "    \"p99_nanos\": {},", arm.p99_nanos)?;
    writeln!(file, "    \"p99_9_nanos\": {},", arm.p99_9_nanos)?;
    writeln!(file, "    \"maximum_nanos\": {},", arm.maximum_nanos)?;
    writeln!(file, "    \"sample_count\": {},", arm.sample_count)?;
    writeln!(file, "    \"gaps\": {},", arm.gaps)?;
    writeln!(file, "    \"overflows\": {},", arm.overflows)?;
    writeln!(file, "    \"recoveries\": {}", arm.recoveries)?;
    write!(file, "  }}")
}

fn json_escape(value: &str) -> String {
    value
        .chars()
        .flat_map(|character| match character {
            '"' => "\\\"".chars().collect::<Vec<_>>(),
            '\\' => "\\\\".chars().collect::<Vec<_>>(),
            '\n' => "\\n".chars().collect::<Vec<_>>(),
            '\r' => "\\r".chars().collect::<Vec<_>>(),
            '\t' => "\\t".chars().collect::<Vec<_>>(),
            character => vec![character],
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn benchmark_fixture_exercises_the_declared_number_of_unique_trades() {
        let message = benchmark_message().expect("benchmark message formats");
        let trades = CoinbaseDecoder::new()
            .decode(message.as_bytes())
            .expect("benchmark message decodes");
        assert_eq!(trades.len(), EVENTS_PER_SAMPLE);
        assert_eq!(trades.first().expect("first trade").trade_id, "benchmark-0");
        assert_eq!(
            trades.last().expect("last trade").trade_id,
            format!("benchmark-{}", EVENTS_PER_SAMPLE - 1)
        );
    }
}
