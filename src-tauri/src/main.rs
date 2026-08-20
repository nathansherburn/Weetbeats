// A release build should not open a console window behind the app.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod audio;
mod commands;
mod state;

use std::sync::Arc;

use state::AppState;

fn main() {
    let state = match AppState::start() {
        Ok(state) => state,
        Err(e) => {
            // Nothing to draw a dialog with yet, so say it plainly and stop.
            eprintln!("Weetbeats could not open an audio device: {e}");
            std::process::exit(1);
        }
    };

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(Arc::new(state))
        .invoke_handler(tauri::generate_handler![
            commands::startup,
            commands::add_instruments,
            commands::add_dropped,
            commands::remove_track,
            commands::set_step,
            commands::set_track_gain,
            commands::set_track_muted,
            commands::set_track_soloed,
            commands::audition,
            commands::set_bpm,
            commands::set_master_gain,
            commands::set_playing,
            commands::panic_stop,
            commands::playhead,
        ])
        .run(tauri::generate_context!())
        .expect("Weetbeats could not start");
}
