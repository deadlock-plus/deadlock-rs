//! Generates the Tauri context: bundles `tauri.conf.json`, the icons and the frontend
//! assets into the binary. Without it the window has nothing to load.

fn main() {
    tauri_build::build();
}
