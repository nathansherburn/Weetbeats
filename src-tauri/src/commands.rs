//! Everything the front end can ask for.
//!
//! These run on the app thread. Anything that reads a file goes through
//! `spawn_blocking` so a big sample cannot stall the window.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Serialize;
use tauri::{AppHandle, Manager, State};
use tauri_plugin_dialog::DialogExt;
use weetbeats_engine::{Command, EngineNote, Note, Project, SampleRef, Track, DEFAULT_PITCH};

use crate::audio::AudioInfo;
use crate::browser::{self, Listing};
use crate::state::AppState;

/// What the front end gets when it starts up.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Startup {
    pub project: Project,
    pub audio: AudioInfo,
    pub samples: Listing,
}

/// A new row, with the waveform to draw in it.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NewTrack {
    pub track: Track,
    pub peaks: Vec<f32>,
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
pub fn startup(app: AppHandle, state: State<'_, Arc<AppState>>) -> Startup {
    let samples = match starter_pack(&app) {
        Some(dir) => browser::scan(&dir),
        None => Listing {
            root: String::new(),
            entries: Vec::new(),
            truncated: false,
        },
    };
    Startup {
        project: state.project.lock().unwrap().clone(),
        audio: state.audio.clone(),
        samples,
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

#[tauri::command]
pub async fn choose_folder(app: AppHandle) -> Option<Listing> {
    // The picker is modal and driven by the main thread, so it is opened from a blocking
    // task. Waiting for it on the main thread would deadlock the window.
    let picked = tauri::async_runtime::spawn_blocking(move || {
        app.dialog()
            .file()
            .set_title("Choose a folder of samples")
            .blocking_pick_folder()
    })
    .await
    .ok()
    .flatten()?;

    let path = picked.into_path().ok()?;
    tauri::async_runtime::spawn_blocking(move || browser::scan(&path))
        .await
        .ok()
}

/// Decoding reads a file, so it happens on a blocking thread. A short drum sample is
/// nothing, but a two minute stereo wav would otherwise hold up every other command.
#[tauri::command]
pub async fn preview(path: String, state: State<'_, Arc<AppState>>) -> Result<(), String> {
    let state = Arc::clone(state.inner());
    tauri::async_runtime::spawn_blocking(move || -> Result<(), String> {
        let sample = state.load_sample(Path::new(&path))?;
        state.send(Command::Preview { sample, gain: 0.9 });
        Ok(())
    })
    .await
    .map_err(|e| format!("could not load the sample: {e}"))?
}

#[tauri::command]
pub async fn add_track(path: String, state: State<'_, Arc<AppState>>) -> Result<NewTrack, String> {
    let state = Arc::clone(state.inner());
    tauri::async_runtime::spawn_blocking(move || add_track_now(&state, PathBuf::from(path)))
        .await
        .map_err(|e| format!("could not load the sample: {e}"))?
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

/// The waveform for a sample, for drawing on a track row.
#[tauri::command]
pub async fn waveform(path: String, state: State<'_, Arc<AppState>>) -> Result<Vec<f32>, String> {
    let state = Arc::clone(state.inner());
    tauri::async_runtime::spawn_blocking(move || {
        state.load_sample(Path::new(&path)).map(|s| s.peaks.clone())
    })
    .await
    .map_err(|e| format!("could not load the sample: {e}"))?
}
