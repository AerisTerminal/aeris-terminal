//! Offline market coordinator baseline. No provider credentials or network.
use std::error::Error;

fn main() -> Result<(), Box<dyn Error>> {
    let mut measurements =
        aeris_market_runtime::market_service::coordinator_performance_baseline()?;
    let bridge = aeris_market_runtime::market_service::bridge_wake_performance()?;
    if let (Some(measurements), Some(bridge)) = (measurements.as_object_mut(), bridge.as_object()) {
        measurements.extend(bridge.clone());
    }
    println!("{}", serde_json::to_string_pretty(&measurements)?);
    Ok(())
}
