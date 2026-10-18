//! Non-shipping, public-feed validation entry point.

mod cli;
mod live;

use std::{error::Error, time::Instant};

use aeris_probes::{Envelope, Evidence, ProbeError, ProbeResult, market};
use serde_json::json;

fn run(
    root: &aeris_platform_runtime::data_root::DataRoot,
    options: &cli::Options,
    evidence: &mut Evidence,
) -> ProbeResult<()> {
    use aeris_platform_runtime::data_root::InstanceLock;
    let _lock =
        InstanceLock::acquire(root).map_err(|error| ProbeError::Runtime(error.to_string()))?;
    let service = market(aeris_market_runtime::MarketService::start())?;
    let result = live::run(&service, options, evidence);
    let started = Instant::now();
    let shutdown = service.shutdown(std::time::Duration::from_secs(2));
    let elapsed = started.elapsed().as_millis();
    let shutdown_check = evidence.check(
        "shutdown_latency_ms",
        shutdown.is_ok() && elapsed < 2_000,
        json!({"elapsed_ms": elapsed}),
    );
    result?;
    market(shutdown)?;
    shutdown_check
}

fn main() -> Result<(), Box<dyn Error>> {
    use aeris_platform_runtime::data_root::{DataRootMode, initialize_data_root};

    let options = cli::Options::parse(std::env::args().skip(1)).map_err(ProbeError::Argument)?;
    // Reject unsafe roots before collecting evidence or attempting to write output.
    let root = initialize_data_root(DataRootMode::Validation {
        root: options.root.clone(),
        borrow_stored_ctrader: false,
    })
    .map_err(|error| ProbeError::Runtime(error.to_string()))?;
    let envelope = Envelope::collect(
        "market_lifecycle",
        options.root.clone(),
        options.out.clone(),
        false,
    )?;
    let started_at = aeris_probes::utc_now().to_rfc3339();
    let mut evidence = Evidence::new();
    let result = run(root, &options, &mut evidence);
    let passed = envelope.finish(&started_at, &mut evidence, &result)?;
    if !passed {
        return Err(Box::new(ProbeError::Invariant(
            "market lifecycle probe failed; see JSON evidence".to_string(),
        )));
    }
    println!("Market lifecycle probe passed: {}", options.out.display());
    Ok(())
}
