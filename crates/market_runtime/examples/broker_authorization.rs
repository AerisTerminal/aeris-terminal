//! Exercises the same runtime-owned browser/vault path as the desktop command.
//! Client secrets are configured privately in AWS; never accepted as arguments.

use aeris_market_runtime::MarketService;
use std::{process::ExitCode, time::Duration};

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).take(2).collect();
    let disconnect = match arguments.as_slice() {
        [] => false,
        [action] if action == "disconnect" => true,
        _ => {
            eprintln!(
                "Usage: cargo run -p aeris_market_runtime --example broker_authorization --locked -- [disconnect]"
            );
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
    let result = if disconnect {
        market.disconnect_provider("tastytrade")
    } else {
        println!("Complete tastytrade authorization in your system browser when it opens.");
        market.connect_provider("tastytrade")
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
