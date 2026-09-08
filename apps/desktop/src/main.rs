#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]

//! Native executable entry point.

mod desktop;

fn main() {
    desktop::run();
}
