//! Exercises the same runtime-owned browser/vault path as the desktop command.
//! Client secrets are configured privately in AWS; never accepted as arguments.
//! Close the desktop before running this example so only one runtime owns sessions.

use aeris_market_runtime::MarketService;
use std::{process::ExitCode, time::Duration};

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let usage = "Usage: broker_authorization [tastytrade|ctrader] [connect|disconnect]";
    let action = match arguments.as_slice() {
        [] => ("tastytrade", "connect"),
        [provider] if matches!(provider.as_str(), "tastytrade" | "ctrader") => {
            (provider.as_str(), "connect")
        }
        [provider, action]
            if matches!(provider.as_str(), "tastytrade" | "ctrader")
                && matches!(action.as_str(), "connect" | "disconnect") =>
        {
            (provider.as_str(), action.as_str())
        }
        // Preserve the existing one-argument tastytrade disconnect command.
        [action] if action == "disconnect" => ("tastytrade", "disconnect"),
        _ => {
            eprintln!("{usage}");
            return ExitCode::FAILURE;
        }
    };
    let market = match MarketService::start() {
        Ok(market) => market,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::FAILURE;
        }
    };
    let result = if action.1 == "disconnect" {
        market.disconnect_provider(action.0)
    } else {
        println!(
            "Complete {} authorization in your system browser when it opens.",
            action.0
        );
        market.connect_provider(action.0)
    };
    let shutdown = market.shutdown(Duration::from_secs(5));
    let mut succeeded = true;
    for outcome in [result, shutdown.map(|()| String::new())] {
        match outcome {
            Ok(message) if !message.is_empty() => println!("{message}"),
            Ok(_) => {}
            Err(error) => {
                eprintln!("{error}");
                succeeded = false;
            }
        }
    }
    if succeeded {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
