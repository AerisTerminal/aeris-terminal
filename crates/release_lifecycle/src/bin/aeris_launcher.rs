#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]

//! Stable packaging launcher/updater. This binary lives outside version directories; the
//! launcher itself is `aeris_release_lifecycle::run_native_launcher`.

fn main() -> std::process::ExitCode {
    aeris_release_lifecycle::run_native_launcher()
}
