#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]

use std::{
    process,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
};

use axiusflow_engine::{
    EngineState, MarketService, bind_listener, default_engine_state_root, serve_client_with_market,
};
use axiusflow_local_engine_client::{ENGINE_SOCKET_NAME, native_installation_token};
use interprocess::local_socket::traits::Listener as _;

fn main() {
    if std::env::args_os().len() != 1 {
        eprintln!("axiusflow_engine does not accept provider or chart commands");
        process::exit(2);
    }
    if let Err(error) = run() {
        eprintln!("Axiusflow engine failed: {error}");
        process::exit(1);
    }
}

fn run() -> Result<(), String> {
    const MAXIMUM_CLIENTS: usize = 4;

    let token = Arc::new(native_installation_token()?);
    let listener = bind_listener(ENGINE_SOCKET_NAME).map_err(|error| error.to_string())?;
    register_engine_autostart();
    let mut epoch_bytes = [0_u8; 8];
    getrandom::fill(&mut epoch_bytes).map_err(|error| error.to_string())?;
    let engine_epoch = u64::from_le_bytes(epoch_bytes).max(1);
    let state = EngineState::open(default_engine_state_root()?)?;
    let market = MarketService::start()?;
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
        let market = market.clone();
        let active_clients = Arc::clone(&active_clients);
        thread::Builder::new()
            .name("axiusflow-engine-client".to_string())
            .spawn(move || {
                if let Err(error) = serve_client_with_market(
                    stream,
                    token.as_slice(),
                    engine_epoch,
                    &state,
                    &market,
                ) {
                    eprintln!("Axiusflow engine rejected a local client: {error}");
                }
                active_clients.fetch_sub(1, Ordering::AcqRel);
            })
            .map_err(|error| error.to_string())?;
    }
}

#[cfg(all(target_os = "windows", not(debug_assertions)))]
fn register_engine_autostart() {
    use std::os::windows::process::CommandExt as _;

    let Ok(executable) = std::env::current_exe() else {
        return;
    };
    let _ = thread::Builder::new()
        .name("axiusflow-engine-autostart".to_string())
        .spawn(move || {
            let command = format!("\"{}\"", executable.display());
            let mut process = std::process::Command::new("reg.exe");
            process.args([
                "add",
                r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run",
                "/v",
                "AxiusflowEngine",
                "/t",
                "REG_SZ",
                "/d",
                &command,
                "/f",
            ]);
            process.creation_flags(0x0800_0000);
            if !matches!(process.status(), Ok(status) if status.success()) {
                eprintln!("Axiusflow engine login startup registration failed");
            }
        });
}

#[cfg(not(all(target_os = "windows", not(debug_assertions))))]
fn register_engine_autostart() {}
