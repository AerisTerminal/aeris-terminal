//! Live market data plane: Coinbase ingest to binary client streams.

mod aggregation;
mod backfill;
mod instruments;
mod provenance;
mod server;

use std::{env, process::ExitCode};

fn main() -> ExitCode {
    match bootstrap() {
        Ok(code) => code,
        Err(error) => {
            eprintln!("axiusflow_market_data_plane fail-closed: {error}");
            ExitCode::FAILURE
        }
    }
}

/// The tungstenite handshake callback trait fixes one `Err` shape the
/// `result_large_err` lint cannot satisfy without reimplementing the handshake.
#[expect(clippy::result_large_err)]
fn bootstrap() -> Result<ExitCode, String> {
    let mut listen = None;
    let mut products = None;
    let mut arguments = env::args().skip(1);
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--listen" => {
                listen = Some(arguments.next().ok_or("--listen requires an address")?);
            }
            "--products" => {
                products = Some(
                    arguments
                        .next()
                        .ok_or("--products requires a comma-separated list")?,
                );
            }
            other => return Err(format!("unsupported argument: {other}")),
        }
    }
    let listen = listen.ok_or("--listen is required")?;
    let products: Vec<String> = products
        .ok_or("--products is required")?
        .split(',')
        .map(str::to_string)
        .collect();

    let plane = server::MarketDataPlane::try_new(&products)?;
    plane.start()?;
    println!(
        "axiusflow_market_data_plane listener_started=true products={} backfill_bars={}",
        products.join(","),
        plane.health().backfill_bars
    );

    let listener = std::net::TcpListener::bind(&listen).map_err(|error| error.to_string())?;
    let shared = std::sync::Arc::new(plane);
    for connection in listener.incoming() {
        let connection = connection.map_err(|error| error.to_string())?;
        let shared = std::sync::Arc::clone(&shared);
        std::thread::spawn(move || {
            let mut requested = String::new();
            let mut callback =
                |request: &tungstenite::handshake::server::Request,
                 response: tungstenite::handshake::server::Response| {
                    requested = request.uri().path().to_string();
                    Ok::<_, tungstenite::handshake::server::ErrorResponse>(response)
                };
            let callback = &mut callback;
            match tungstenite::accept_hdr(connection, callback) {
                Ok(mut websocket) => {
                    let product = requested.trim_start_matches('/').to_string();
                    if let Err(error) = shared.serve_client(&product, &mut websocket) {
                        eprintln!("client session for {product} failed: {error}");
                    }
                }
                Err(error) => eprintln!("websocket handshake failed: {error}"),
            }
        });
    }
    Ok(ExitCode::SUCCESS)
}
