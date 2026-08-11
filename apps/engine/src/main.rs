#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]

use std::{
    ffi::{OsStr, OsString},
    process,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use axiusflow_engine::{
    EngineShutdown, EngineState, MarketService, bind_listener, default_engine_state_root,
    serve_client_with_market_and_shutdown,
};
use axiusflow_local_engine_client::{ENGINE_SOCKET_NAME, EngineClient, native_installation_token};
use interprocess::local_socket::{ListenerNonblockingMode, traits::Listener as _};

fn main() {
    let command = match parse_command(std::env::args_os().skip(1)) {
        Ok(command) => command,
        Err(error) => {
            eprintln!("{error}");
            process::exit(2);
        }
    };
    let result = match command {
        EngineCommand::Run => run(),
        EngineCommand::Shutdown => shutdown_running_engine(),
    };
    if let Err(error) = result {
        eprintln!("Axiusflow engine failed: {error}");
        process::exit(1);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EngineCommand {
    Run,
    Shutdown,
}

fn parse_command(mut arguments: impl Iterator<Item = OsString>) -> Result<EngineCommand, String> {
    match (arguments.next(), arguments.next()) {
        (None, None) => Ok(EngineCommand::Run),
        (Some(argument), None) if argument == OsStr::new("--shutdown") => {
            Ok(EngineCommand::Shutdown)
        }
        _ => Err("usage: axiusflow_engine [--shutdown]".to_string()),
    }
}

fn shutdown_running_engine() -> Result<(), String> {
    let token = native_installation_token()?;
    EngineClient::connect(ENGINE_SOCKET_NAME, token.as_slice())?.shutdown_engine()
}

fn run() -> Result<(), String> {
    const MAXIMUM_CLIENTS: usize = 4;
    const ACCEPT_POLL_INTERVAL: Duration = Duration::from_millis(10);
    const ENGINE_SHUTDOWN_DEADLINE: Duration = Duration::from_secs(2);

    let token = Arc::new(native_installation_token()?);
    let listener = bind_listener(ENGINE_SOCKET_NAME).map_err(|error| error.to_string())?;
    listener
        .set_nonblocking(ListenerNonblockingMode::Accept)
        .map_err(|error| error.to_string())?;
    let mut epoch_bytes = [0_u8; 8];
    getrandom::fill(&mut epoch_bytes).map_err(|error| error.to_string())?;
    let engine_epoch = u64::from_le_bytes(epoch_bytes).max(1);
    let state = EngineState::open(default_engine_state_root()?)?;
    let market = MarketService::start()?;
    let active_clients = Arc::new(AtomicUsize::new(0));
    let shutdown = EngineShutdown::default();

    while !shutdown.is_requested() {
        let stream = match listener.accept() {
            Ok(stream) => stream,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(ACCEPT_POLL_INTERVAL);
                continue;
            }
            Err(error) => return Err(error.to_string()),
        };
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
        let market = market.clone();
        let active_clients = Arc::clone(&active_clients);
        let shutdown = shutdown.clone();
        thread::Builder::new()
            .name("axiusflow-engine-client".to_string())
            .spawn(move || {
                if let Err(error) = serve_client_with_market_and_shutdown(
                    stream,
                    token.as_slice(),
                    engine_epoch,
                    &state,
                    &market,
                    &shutdown,
                ) {
                    eprintln!("Axiusflow engine rejected a local client: {error}");
                }
                active_clients.fetch_sub(1, Ordering::AcqRel);
            })
            .map_err(|error| error.to_string())?;
    }
    drop(listener);
    let deadline = Instant::now() + ENGINE_SHUTDOWN_DEADLINE;
    let market_shutdown = market.shutdown(deadline.saturating_duration_since(Instant::now()));
    while active_clients.load(Ordering::Acquire) != 0 && Instant::now() < deadline {
        thread::sleep(ACCEPT_POLL_INTERVAL);
    }
    market_shutdown
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::{EngineCommand, parse_command};

    #[test]
    fn lifecycle_command_line_accepts_only_run_or_complete_shutdown() {
        assert_eq!(
            parse_command(Vec::new().into_iter()),
            Ok(EngineCommand::Run)
        );
        assert_eq!(
            parse_command(vec![OsString::from("--shutdown")].into_iter()),
            Ok(EngineCommand::Shutdown)
        );
        assert!(parse_command(vec![OsString::from("--provider")].into_iter()).is_err());
        assert!(
            parse_command(vec![OsString::from("--shutdown"), OsString::from("extra")].into_iter())
                .is_err()
        );
    }
}
