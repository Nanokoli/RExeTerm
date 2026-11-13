#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(windows)]
mod windows;

#[cfg(windows)]
fn main() {
    windows::run();
}

#[cfg(not(windows))]
fn main() {
    eprintln!(
        "This first Tauri preview targets Windows; other desktop platforms are not yet enabled."
    );
    std::process::exit(1);
}
