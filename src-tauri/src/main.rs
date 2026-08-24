// A release build should not open a console window behind the app.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod audio;
mod commands;
mod menu;
mod plugins;
mod state;

use std::sync::Arc;

use state::AppState;
use tauri::Manager;

fn main() {
    let state = match AppState::start() {
        Ok(state) => Arc::new(state),
        Err(e) => {
            // Nothing to draw a dialog with yet, so say it plainly and stop.
            eprintln!("Weetbeats could not open an audio device: {e}");
            std::process::exit(1);
        }
    };
    state::spawn_saver(Arc::clone(&state));

    let on_close = Arc::clone(&state);
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(state)
        .setup(|app| {
            menu::install(app.handle())?;
            // Runs on the main thread, which is the whole point: the plugin studio has to
            // live there, because a plugin's window can only be made there. Anything the
            // project was already using is loaded now, since until this moment there was
            // nowhere to put it.
            app.state::<Arc<AppState>>().attach(app.handle().clone());
            Ok(())
        })
        .on_menu_event(menu::handle)
        .on_window_event(move |_window, event| {
            // The saver writes every second or so anyway; this is so closing the window
            // never loses the last thing you did.
            if matches!(event, tauri::WindowEvent::CloseRequested { .. }) {
                let _ = on_close.save_now();
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::startup,
            commands::add_instruments,
            commands::add_dropped,
            commands::remove_track,
            commands::set_step,
            commands::set_pattern_gain,
            commands::set_pattern_muted,
            commands::mute_pattern,
            commands::set_pattern_soloed,
            commands::audition,
            commands::set_pattern_pitched,
            commands::set_voicing,
            commands::list_plugins,
            commands::add_plugin,
            commands::plugin_params,
            commands::set_plugin_param,
            commands::set_plugin_window,
            commands::plugin_window_open,
            commands::set_note,
            commands::clear_note,
            commands::move_note,
            commands::edit_notes,
            commands::add_pattern,
            commands::duplicate_pattern,
            commands::remove_pattern,
            commands::rename_pattern,
            commands::set_pattern_steps,
            commands::open_pattern,
            commands::close_pattern,
            commands::undo,
            commands::redo,
            commands::set_pattern_colour,
            commands::rename_project,
            commands::place_pattern,
            commands::move_placement,
            commands::resize_placement,
            commands::clear_song_bar,
            commands::seek_song,
            commands::set_bpm,
            commands::set_playing,
            commands::panic_stop,
            commands::playhead,
        ])
        .run(tauri::generate_context!())
        .expect("Weetbeats could not start");
}
