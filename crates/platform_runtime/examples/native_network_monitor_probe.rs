use tradingplot_platform_runtime::NativeNetworkMonitor;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let monitor = NativeNetworkMonitor::connect()?;
    println!("native_network_monitor_current={:?}", monitor.current());
    Ok(())
}
