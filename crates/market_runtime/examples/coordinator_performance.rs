//! Offline market coordinator baseline. No provider credentials or network.
use std::error::Error;

fn main() -> Result<(), Box<dyn Error>> {
    let measurements = aeris_market_runtime::market_service::coordinator_performance_baseline()?;
    println!("{}", serde_json::to_string_pretty(&measurements)?);
    Ok(())
}
