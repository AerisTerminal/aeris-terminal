use std::{env, process};

use axiusflow_engine::{ENGINE_SOCKET_NAME, MINIMUM_TOKEN_BYTES, bind_listener, serve_client};
use interprocess::local_socket::traits::Listener as _;

fn main() {
    if let Err(error) = run() {
        eprintln!("Axiusflow engine failed: {error}");
        process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let token = env::var_os("AXIUSFLOW_ENGINE_TOKEN")
        .ok_or_else(|| "AXIUSFLOW_ENGINE_TOKEN is not configured".to_string())?
        .into_encoded_bytes();
    if token.len() < MINIMUM_TOKEN_BYTES {
        return Err(format!(
            "AXIUSFLOW_ENGINE_TOKEN must contain at least {MINIMUM_TOKEN_BYTES} bytes"
        ));
    }
    let listener = bind_listener(ENGINE_SOCKET_NAME).map_err(|error| error.to_string())?;
    let mut epoch_bytes = [0_u8; 8];
    getrandom::fill(&mut epoch_bytes).map_err(|error| error.to_string())?;
    let engine_epoch = u64::from_le_bytes(epoch_bytes).max(1);
    loop {
        let stream = listener.accept().map_err(|error| error.to_string())?;
        if let Err(error) = serve_client(stream, &token, engine_epoch) {
            eprintln!("Axiusflow engine rejected a local client: {error}");
        }
    }
}
