#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]
#![deny(clippy::let_underscore_must_use, clippy::let_underscore_future)]

//! Native executable entry point.

mod desktop;

fn main() {
    let parsed =
        match aeris_platform_runtime::parse_data_root_arguments(std::env::args_os().skip(1)) {
            Ok(parsed) => parsed,
            Err(error) => {
                let message = format!("Aeris startup arguments are invalid: {error}");
                aeris_observability::diagnostic!("{message}");
                if let Err(error) = aeris_platform_runtime::message_box("Aeris Terminal", &message)
                {
                    aeris_observability::diagnostic!("Aeris startup dialog failed: {error}");
                }
                desktop::flush_diagnostics_before_exit();
                std::process::exit(2);
            }
        };
    let readiness = parsed
        .command_arguments
        .iter()
        .any(|argument| argument == "--desktop-readiness");
    let data_root = match aeris_platform_runtime::initialize_data_root(parsed.mode) {
        Ok(root) => root,
        Err(error) => {
            let message = format!("Aeris diagnostics path is unavailable: {error}");
            aeris_observability::diagnostic!("{message}");
            if let Err(error) = aeris_platform_runtime::message_box("Aeris Terminal", &message) {
                aeris_observability::diagnostic!("Aeris startup dialog failed: {error}");
            }
            desktop::flush_diagnostics_before_exit();
            std::process::exit(1);
        }
    };
    let instance_lock = match aeris_platform_runtime::InstanceLock::acquire(data_root) {
        Ok(lock) => lock,
        Err(aeris_platform_runtime::DataRootError::AlreadyRunning) if !readiness => {
            if let Err(error) = aeris_platform_runtime::message_box(
                "Aeris Terminal",
                "Aeris is already running for this data folder",
            ) {
                aeris_observability::diagnostic!("Aeris already-running dialog failed: {error}");
            }
            return;
        }
        Err(error) => {
            aeris_observability::diagnostic!("Aeris instance lock failed: {error}");
            desktop::flush_diagnostics_before_exit();
            std::process::exit(1);
        }
    };
    let data_root = data_root.path().to_path_buf();
    let logs_directory = data_root.join("logs");
    let log_path = logs_directory.join("aeris.log");
    let _log_guard = match aeris_observability::install_log_sink(&log_path) {
        Ok(guard) => guard,
        Err(error) => {
            let message = format!(
                "Aeris diagnostics could not start: {error}\n\nLog path: {}",
                log_path.display()
            );
            aeris_observability::diagnostic!("{message}");
            if let Err(error) = aeris_platform_runtime::message_box("Aeris Terminal", &message) {
                aeris_observability::diagnostic!(
                    "Aeris diagnostics startup dialog failed: {error}"
                );
            }
            return;
        }
    };
    let release = aeris_release_lifecycle::current_release_identity();
    if let Err(error) = aeris_observability::install_panic_hook(
        &logs_directory,
        release.release_identity,
        release.install_generation,
    ) {
        aeris_observability::diagnostic!("Aeris crash diagnostics could not start: {error}");
    }

    desktop::run(parsed.command_arguments, instance_lock);
}
