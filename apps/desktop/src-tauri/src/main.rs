// Hide the console window on Windows in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    if let Err(error) = tether_desktop_lib::run() {
        tracing::error!(%error, "Tether failed");
        std::process::exit(1);
    }
}
