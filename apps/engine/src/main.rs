use std::{
    process,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
};

use axiusflow_engine::{
    ENGINE_SOCKET_NAME, EngineState, bind_listener, native_installation_token,
    serve_client_with_state,
};
use interprocess::local_socket::traits::Listener as _;

fn main() {
    if let Err(error) = run() {
        eprintln!("Axiusflow engine failed: {error}");
        process::exit(1);
    }
}

fn run() -> Result<(), String> {
    const MAXIMUM_CLIENTS: usize = 4;

    let token = Arc::new(native_installation_token()?);
    let listener = bind_listener(ENGINE_SOCKET_NAME).map_err(|error| error.to_string())?;
    let mut epoch_bytes = [0_u8; 8];
    getrandom::fill(&mut epoch_bytes).map_err(|error| error.to_string())?;
    let engine_epoch = u64::from_le_bytes(epoch_bytes).max(1);
    let state = EngineState::default();
    let active_clients = Arc::new(AtomicUsize::new(0));
    loop {
        let stream = listener.accept().map_err(|error| error.to_string())?;
        if active_clients
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                (active < MAXIMUM_CLIENTS).then_some(active + 1)
            })
            .is_err()
        {
            drop(stream);
            continue;
        }
        let token = Arc::clone(&token);
        let state = state.clone();
        let active_clients = Arc::clone(&active_clients);
        thread::Builder::new()
            .name("axiusflow-engine-client".to_string())
            .spawn(move || {
                if let Err(error) =
                    serve_client_with_state(stream, token.as_slice(), engine_epoch, &state)
                {
                    eprintln!("Axiusflow engine rejected a local client: {error}");
                }
                active_clients.fetch_sub(1, Ordering::AcqRel);
            })
            .map_err(|error| error.to_string())?;
    }
}
