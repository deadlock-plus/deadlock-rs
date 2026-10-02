//! DeadRS Overlay: a small second-monitor window over `deadlock-rs`.
//!
//! Read-only. Everything shown is information the game already puts on your screen; it is
//! collected here so it can sit on a second monitor instead of being paged through.

// A console window behind a GUI on Windows is noise, but keep it in debug builds where
// the poller's errors are worth seeing.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod model;
mod poll;
mod rank_art;
mod statlocker;

fn main() {
    tauri::Builder::default()
        .setup(|app| {
            poll::spawn(app.handle().clone());
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("failed to start DeadRS Overlay");
}
