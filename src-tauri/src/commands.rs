//! Everything the front end can ask for.
//!
//! These run on the app thread. Anything that reads a file goes through
//! `spawn_blocking` so a big sample cannot stall the window.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Serialize;
use tauri::{AppHandle, Manager, State};
use tauri_plugin_dialog::DialogExt;
use weetbeats_engine::sample::{is_audio_file, AUDIO_EXTENSIONS};
use weetbeats_engine::{Command, EngineNote, Note, Project, SampleRef, Track, DEFAULT_PITCH};

use crate::audio::AudioInfo;
use crate::state::AppState;

/// What the front end gets when it starts up.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Startup {
    pub project: Project,
    pub audio: AudioInfo,
}

/// A new row, with the waveform to draw in it.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NewTrack {
    pub track: Track,
    pub peaks: Vec<f32>,
}

/// The result of one trip to the file picker. Files that would not decode are reported
/// rather than silently skipped, and they do not stop the ones that would.
#[derive(Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Added {
    pub tracks: Vec<NewTrack>,
    pub failed: Vec<String>,
}

/// Polled every frame while playing. Kept small on purpose.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayheadPayload {
    pub playing: bool,
    pub step: u32,
    pub progress: f32,
    pub voices: u32,
    pub peak: f32,
    pub stream_errors: u32,
}

#[tauri::command]
pub fn startup(state: State<'_, Arc<AppState>>) -> Startup {
    Startup {
        project: state.project.lock().unwrap().clone(),
        audio: state.audio.clone(),
    }
}

/// Where the drums that ship with the app live.
///
/// Bundled as a resource under `drums`, so in a built `.app` they sit inside it. The name
/// differs from the folder in the repo because Tauri stages resources into the Cargo target
/// directory, where `starter-pack` is already taken by the tool that generates them.
///
/// Running from a checkout there is no bundle, so fall back to the folder in the repo.
fn starter_pack(app: &AppHandle) -> Option<PathBuf> {
    if let Ok(path) = app
        .path()
        .resolve("drums", tauri::path::BaseDirectory::Resource)
    {
        if path.is_dir() {
            return Some(path);
        }
    }
    let in_repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../assets/starter-pack");
    in_repo.is_dir().then_some(in_repo)
}

/// Open the system file picker and turn everything chosen into a track.
///
/// The picker is modal and driven by the main thread, so it is opened from a blocking
/// task. Waiting for it on the main thread would deadlock the window.
///
/// Multi-select is on, because adding a kit is the normal case and one file at a time
/// would be four trips through the dialog.
#[tauri::command]
pub async fn add_instruments(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
) -> Result<Added, String> {
    let state = Arc::clone(state.inner());
    // Open where they were last, or at the drums that ship with the app the first time,
    // which is the only thing keeping the starter pack discoverable now the browser is gone.
    let start_dir = state.last_folder().or_else(|| starter_pack(&app));

    let picked = tauri::async_runtime::spawn_blocking(move || {
        let mut dialog = app
            .dialog()
            .file()
            .set_title("Add an instrument")
            .add_filter("Audio", AUDIO_EXTENSIONS);
        if let Some(dir) = start_dir {
            dialog = dialog.set_directory(dir);
        }
        dialog.blocking_pick_files()
    })
    .await
    .map_err(|e| format!("the file picker did not open: {e}"))?;

    let Some(picked) = picked else {
        // Cancelled. Not an error, just nothing to add.
        return Ok(Added::default());
    };

    let paths: Vec<PathBuf> = picked
        .into_iter()
        .filter_map(|p| p.into_path().ok())
        .collect();
    tauri::async_runtime::spawn_blocking(move || add_all(&state, paths))
        .await
        .map_err(|e| format!("could not load the samples: {e}"))
}

/// Add every path that decodes, and say what happened to the ones that do not.
///
/// Nothing here is silent. A drop that quietly does nothing is worse than an error,
/// because there is no way to tell it apart from the app being broken.
fn add_all(state: &AppState, paths: Vec<PathBuf>) -> Added {
    let mut added = Added::default();
    if let Some(folder) = paths.first().and_then(|p| p.parent()) {
        state.remember_folder(folder);
    }
    for path in paths {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("that")
            .to_string();
        if path.is_dir() {
            added.failed.push(format!(
                "{name} is a folder — open it and pick the sounds inside"
            ));
        } else if !is_audio_file(&path) {
            added.failed.push(format!("{name} is not a sound file"));
        } else {
            match add_track_now(state, path) {
                Ok(track) => added.tracks.push(track),
                Err(e) => added.failed.push(e),
            }
        }
    }
    added
}

/// Files dropped onto the window from the Finder. Same job as the picker, different door.
#[tauri::command]
pub async fn add_dropped(
    paths: Vec<String>,
    state: State<'_, Arc<AppState>>,
) -> Result<Added, String> {
    let state = Arc::clone(state.inner());
    let paths: Vec<PathBuf> = paths.into_iter().map(PathBuf::from).collect();
    tauri::async_runtime::spawn_blocking(move || add_all(&state, paths))
        .await
        .map_err(|e| format!("could not load the samples: {e}"))
}

fn add_track_now(state: &AppState, path: PathBuf) -> Result<NewTrack, String> {
    let sample = state.load_sample(&path)?;

    let mut project = state.project.lock().unwrap();
    let used = project.pattern.tracks.len();
    let id = project
        .free_track_id()
        .ok_or_else(|| format!("that is {used} tracks, which is all of them"))?;

    let track = Track::new(
        id,
        sample.name.clone(),
        Some(SampleRef {
            path: path.display().to_string(),
            name: sample.name.clone(),
        }),
    );

    state.send(Command::AddTrack {
        track: id,
        gain: track.gain,
    });
    state.send(Command::SetTrackSample {
        track: id,
        sample: Some(Arc::clone(&sample)),
    });

    project.pattern.tracks.push(track.clone());
    Ok(NewTrack {
        track,
        peaks: sample.peaks.clone(),
    })
}

#[tauri::command]
pub fn remove_track(id: u16, state: State<'_, Arc<AppState>>) {
    let mut project = state.project.lock().unwrap();
    project.pattern.tracks.retain(|t| t.id != id);
    state.send(Command::RemoveTrack { track: id });
}

/// Tick or untick a step. Returns what the box actually is now.
#[tauri::command]
pub fn set_step(id: u16, step: u32, on: bool, state: State<'_, Arc<AppState>>) -> bool {
    let mut project = state.project.lock().unwrap();
    let Some(track) = project.track_mut(id) else {
        return false;
    };
    let now_on = track.set_step(step, on);

    // The engine holds the same note, not a boolean, so the piano roll can send richer
    // ones later without either side changing.
    if now_on {
        let note = Note::step_note(step);
        state.send(Command::SetNote {
            track: id,
            note: EngineNote {
                step: note.step as u16,
                pitch: note.pitch,
                velocity: note.velocity,
                length: note.length as u16,
            },
        });
    } else {
        state.send(Command::ClearNote {
            track: id,
            step: step as u16,
            pitch: DEFAULT_PITCH,
        });
    }
    now_on
}

#[tauri::command]
pub fn set_track_gain(id: u16, gain: f32, state: State<'_, Arc<AppState>>) {
    let mut project = state.project.lock().unwrap();
    if let Some(track) = project.track_mut(id) {
        track.gain = gain.clamp(0.0, 1.5);
        state.send(Command::SetTrackGain {
            track: id,
            gain: track.gain,
        });
    }
}

#[tauri::command]
pub fn set_track_muted(id: u16, muted: bool, state: State<'_, Arc<AppState>>) {
    let mut project = state.project.lock().unwrap();
    if let Some(track) = project.track_mut(id) {
        track.muted = muted;
        state.send(Command::SetTrackMuted { track: id, muted });
    }
}

#[tauri::command]
pub fn set_track_soloed(id: u16, soloed: bool, state: State<'_, Arc<AppState>>) {
    let mut project = state.project.lock().unwrap();
    if let Some(track) = project.track_mut(id) {
        track.soloed = soloed;
        state.send(Command::SetTrackSoloed { track: id, soloed });
    }
}

/// Hear a track without waiting for its next step.
#[tauri::command]
pub fn audition(id: u16, state: State<'_, Arc<AppState>>) {
    state.send(Command::Audition {
        track: id,
        pitch: DEFAULT_PITCH,
        velocity: 110,
    });
}

#[tauri::command]
pub fn set_bpm(bpm: f32, state: State<'_, Arc<AppState>>) -> f32 {
    let mut project = state.project.lock().unwrap();
    project.bpm = bpm.clamp(40.0, 240.0);
    state.send(Command::SetBpm(project.bpm));
    project.bpm
}

#[tauri::command]
pub fn set_master_gain(gain: f32, state: State<'_, Arc<AppState>>) {
    let mut project = state.project.lock().unwrap();
    project.master_gain = gain.clamp(0.0, 1.5);
    state.send(Command::SetMasterGain(project.master_gain));
}

#[tauri::command]
pub fn set_playing(playing: bool, state: State<'_, Arc<AppState>>) {
    state.send(Command::SetPlaying(playing));
}

#[tauri::command]
pub fn panic_stop(state: State<'_, Arc<AppState>>) {
    state.send(Command::StopAll);
}

/// Polled from `requestAnimationFrame`. Reads atomics the audio thread wrote, and takes
/// the chance to drop anything the audio thread handed back.
#[tauri::command]
pub fn playhead(state: State<'_, Arc<AppState>>) -> PlayheadPayload {
    state.take_out_the_trash();
    let playhead = state.shared.playhead();
    PlayheadPayload {
        playing: playhead.playing,
        step: playhead.step,
        progress: playhead.progress,
        voices: playhead.active_voices,
        peak: playhead.peak,
        stream_errors: state.stream_errors(),
    }
}
