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
    EngineShutdown, EngineState, MarketService, SessionPairer, bind_listener,
    default_engine_state_root, serve_client_with_account_gate, start_account_market_gate,
};
use axiusflow_engine_protocol::ResourceMode;
use axiusflow_local_engine_client::{
    ENGINE_SOCKET_NAME, native_installation_token, shutdown_running_engine,
};
use axiusflow_platform_runtime::{
    BackgroundService, NativeSessionShutdownCancellation, NativeSessionShutdownMonitor,
    SessionShutdownError,
};
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

struct SessionShutdownRuntime {
    cancellation: NativeSessionShutdownCancellation,
    worker: thread::JoinHandle<()>,
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

fn run() -> Result<(), String> {
    // Each session owns two streams, so the connection budget counts both
    // halves of up to four concurrent paired sessions.
    const MAXIMUM_CLIENTS: usize = 8;
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
    let workspace = state.workspace();
    install_background_service(&state, workspace.autostart_enabled)?;
    // Never restore hot demand or allow provider activity before the account
    // service has verified an online session or a valid offline lease.
    let mut gated_workspace = workspace.clone();
    gated_workspace.hot_series.clear();
    gated_workspace.resource_mode = ResourceMode::OfflineSuspended as i32;
    let market = MarketService::start(&gated_workspace)?;
    market.set_resource_mode(ResourceMode::OfflineSuspended)?;
    state.set_resource_mode(ResourceMode::OfflineSuspended);
    let active_clients = Arc::new(AtomicUsize::new(0));
    let pairer = SessionPairer::new();
    let shutdown = EngineShutdown::default();
    let account_gate = start_account_market_gate(&state, &market, &shutdown)?;
    let session_shutdown = match start_session_shutdown_monitor(shutdown.clone()) {
        Ok(runtime) => Some(runtime),
        Err(error) => {
            eprintln!("Axiusflow engine session-shutdown integration degraded: {error}");
            None
        }
    };

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
        let pairer = pairer.clone();
        let shutdown = shutdown.clone();
        thread::Builder::new()
            .name("axiusflow-engine-client".to_string())
            .spawn(move || {
                match pairer.accept_one(stream, token.as_slice()) {
                    Ok(Some(pair)) => {
                        if let Err(error) = serve_client_with_account_gate(
                            pair,
                            engine_epoch,
                            &state,
                            &market,
                            &shutdown,
                        ) {
                            eprintln!("Axiusflow engine rejected a local client: {error}");
                        }
                    }
                    Ok(None) => {}
                    Err(error) => {
                        eprintln!("Axiusflow engine rejected a local client: {error}");
                    }
                }
                active_clients.fetch_sub(1, Ordering::AcqRel);
            })
            .map_err(|error| error.to_string())?;
    }
    drop(listener);
    finish_shutdown(
        &state,
        &market,
        active_clients.as_ref(),
        session_shutdown,
        account_gate,
        ACCEPT_POLL_INTERVAL,
        Instant::now() + ENGINE_SHUTDOWN_DEADLINE,
    )
}

fn finish_shutdown(
    state: &EngineState,
    market: &MarketService,
    active_clients: &AtomicUsize,
    session_shutdown: Option<SessionShutdownRuntime>,
    account_gate: thread::JoinHandle<()>,
    poll_interval: Duration,
    deadline: Instant,
) -> Result<(), String> {
    state.begin_shutdown();
    state.set_resource_mode(ResourceMode::OfflineSuspended);
    let session_shutdown = stop_session_shutdown_monitor(session_shutdown, deadline);
    let account_gate_shutdown = account_gate
        .join()
        .map_err(|_| "account market gate stopped unexpectedly".to_string());
    let hot_set_state = state.clone();
    let hot_set_flush = thread::Builder::new()
        .name("axiusflow-engine-hot-set-flush".to_string())
        .spawn(move || hot_set_state.persist_shutdown_hot_set().map(|_| ()))
        .map_err(|error| error.to_string());
    let market_shutdown = market.shutdown(deadline.saturating_duration_since(Instant::now()));
    let hot_set_shutdown = match hot_set_flush {
        Ok(worker) => finish_hot_set_flush(worker, deadline),
        Err(error) => Err(format!("hot-set flush worker could not start: {error}")),
    };
    while active_clients.load(Ordering::Acquire) != 0 && Instant::now() < deadline {
        thread::sleep(poll_interval);
    }
    let client_shutdown = match active_clients.load(Ordering::Acquire) {
        0 => Ok(()),
        active => Err(format!(
            "engine shutdown deadline expired with {active} active client sessions"
        )),
    };
    let errors = [
        session_shutdown,
        account_gate_shutdown,
        market_shutdown,
        hot_set_shutdown,
        client_shutdown,
    ]
    .into_iter()
    .filter_map(Result::err)
    .collect::<Vec<_>>();
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

fn start_session_shutdown_monitor(
    shutdown: EngineShutdown,
) -> Result<SessionShutdownRuntime, String> {
    let monitor = NativeSessionShutdownMonitor::connect().map_err(|error| error.to_string())?;
    let cancellation = monitor.cancellation();
    let worker = thread::Builder::new()
        .name("axiusflow-engine-session-shutdown".to_string())
        .spawn(move || match monitor.wait_for_shutdown() {
            Ok(()) => shutdown.request(),
            Err(SessionShutdownError::Cancelled) => {}
            Err(error) => eprintln!("Axiusflow engine session-shutdown monitor failed: {error}"),
        })
        .map_err(|error| error.to_string())?;
    Ok(SessionShutdownRuntime {
        cancellation,
        worker,
    })
}

fn stop_session_shutdown_monitor(
    runtime: Option<SessionShutdownRuntime>,
    deadline: Instant,
) -> Result<(), String> {
    let Some(runtime) = runtime else {
        return Ok(());
    };
    runtime.cancellation.cancel();
    finish_named_worker(runtime.worker, deadline, "engine session-shutdown monitor")
}

fn install_background_service(state: &EngineState, autostart_enabled: bool) -> Result<(), String> {
    let service = BackgroundService::new(
        std::env::current_exe().map_err(|_| "engine executable path is unavailable".to_string())?,
    )
    .map_err(|error| error.to_string())?;
    if service
        .autostart_enabled()
        .map_err(|error| error.to_string())?
        != autostart_enabled
    {
        service
            .set_autostart(autostart_enabled)
            .map_err(|error| error.to_string())?;
    }
    state.install_background_service(service);
    Ok(())
}

fn finish_hot_set_flush(
    worker: thread::JoinHandle<Result<(), String>>,
    deadline: Instant,
) -> Result<(), String> {
    while !worker.is_finished() {
        let now = Instant::now();
        if now >= deadline {
            return Err("engine hot-set flush exceeded the shutdown deadline".to_string());
        }
        thread::sleep(Duration::from_millis(5).min(deadline.duration_since(now)));
    }
    worker
        .join()
        .map_err(|_| "engine hot-set flush worker panicked".to_string())?
}

fn finish_named_worker(
    worker: thread::JoinHandle<()>,
    deadline: Instant,
    name: &str,
) -> Result<(), String> {
    while !worker.is_finished() {
        let now = Instant::now();
        if now >= deadline {
            return Err(format!("{name} exceeded the shutdown deadline"));
        }
        thread::sleep(Duration::from_millis(5).min(deadline.duration_since(now)));
    }
    worker.join().map_err(|_| format!("{name} panicked"))
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
