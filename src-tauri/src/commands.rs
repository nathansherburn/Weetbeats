//! Everything the front end can ask for.
//!
//! These run on the app thread. Anything that reads a file or opens a dialog goes through
//! `spawn_blocking` so a big sample cannot stall the window.
//!
//! Every one of these that changes the project does three things in the same order: change
//! the project, tell the audio thread, mark the project for saving.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_dialog::DialogExt;
use weetbeats_engine::folder;
use weetbeats_engine::sample::{is_audio_file, AUDIO_EXTENSIONS};
use weetbeats_engine::{
    Command, EngineNote, Note, Pattern, Placement, Project, SampleRef, Track, DEFAULT_PITCH,
};

use crate::state::AppState;

/// What the front end gets when it starts up, and again when a project is opened.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Startup {
    pub project: Project,
    /// The project's name, which is its folder's.
    pub name: String,
    /// Where it lives, for showing on hover.
    pub folder: String,
    /// The little waveforms the track rows draw. Not part of the project: they come from
    /// the samples, and the front end has no way to work them out for itself.
    pub waveforms: Vec<Waveform>,
    /// Anything that went wrong opening it. Shown once.
    pub message: Option<String>,
}

/// The shape of one track's sample, for drawing.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Waveform {
    pub track: u16,
    pub peaks: Vec<f32>,
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

/// The patterns and the song, whole. Anything that adds, copies or deletes a pattern hands
/// both back rather than describing what it did, so the front end cannot drift out of step.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Arrangement {
    pub patterns: Vec<Pattern>,
    pub song: Vec<Placement>,
}

/// What came of putting a note down: whether it went in — a pattern holds only so many —
/// and how long the pattern is now, since a note past the end makes it longer.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NotePut {
    pub fits: bool,
    pub steps: u32,
}

impl NotePut {
    fn nowhere() -> Self {
        NotePut {
            fits: false,
            steps: 0,
        }
    }
}

/// Where a note is: the two things that identify one inside a lane.
#[derive(Deserialize, Clone, Copy)]
#[serde(rename_all = "camelCase")]
pub struct At {
    pub step: u32,
    pub pitch: u8,
}

/// Polled every frame while playing. Kept small on purpose.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayheadPayload {
    pub playing: bool,
    pub step: u32,
    pub progress: f32,
    /// Which patterns are sounding, one bit each.
    pub patterns: u32,
    pub voices: u32,
    pub peak: f32,
    pub stream_errors: u32,
    /// Set when the project could not be written. Nothing else tells you that.
    pub save_error: Option<String>,
}

/// Everything the front end needs to draw a project it has not seen before.
///
/// The samples are all in the cache by now — opening a project decodes them — so this is
/// a few clones, not a read of the disk.
fn startup_payload(state: &AppState, message: Option<String>) -> Startup {
    let dir = state.dir();
    let waveforms = {
        let project = state.project.lock().unwrap();
        project
            .tracks
            .iter()
            .filter_map(|track| {
                let reference = track.sample.as_ref()?;
                let path = folder::resolve(&dir, &reference.path).ok()?;
                let sample = state.load_sample(&path).ok()?;
                Some(Waveform {
                    track: track.id,
                    peaks: sample.peaks.clone(),
                })
            })
            .collect()
    };
    Startup {
        project: state.project.lock().unwrap().clone(),
        name: state.name(),
        folder: dir.display().to_string(),
        waveforms,
        message,
    }
}

fn arrangement(state: &AppState) -> Arrangement {
    let project = state.project.lock().unwrap();
    Arrangement {
        patterns: project.patterns.clone(),
        song: project.song.clone(),
    }
}

#[tauri::command]
pub fn startup(state: State<'_, Arc<AppState>>) -> Startup {
    let message = state.complaint.lock().unwrap().take();
    startup_payload(&state, message)
}

// --- instruments ------------------------------------------------------------

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

/// Add every path that decodes, and say what happened to the ones that do not.
///
/// Nothing here is silent. A drop that quietly does nothing is worse than an error,
/// because there is no way to tell it apart from the app being broken.
fn add_all(state: &AppState, paths: Vec<PathBuf>) -> Added {
    state.remember("tracks");
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
    if !added.tracks.is_empty() {
        state.touch();
    }
    added
}

/// Copy a sample into the project, decode it, and give it a track.
///
/// The copy comes first. From that moment the project owns the sound: moving or deleting
/// the file it came from cannot break the project, and there is no save step where it
/// might not have been gathered up yet.
fn add_track_now(state: &AppState, source: PathBuf) -> Result<NewTrack, String> {
    // Nothing gets copied for a track we cannot make.
    let free = {
        let project = state.project.lock().unwrap();
        project.free_track_id().ok_or_else(|| {
            format!(
                "that is {} tracks, which is all of them",
                project.tracks.len()
            )
        })?
    };

    let dir = state.dir();
    let imported = folder::import_sample(&dir, &source)?;
    let path = folder::resolve(&dir, &imported.path)?;
    if !imported.reused {
        // A new file, at a name the folder may well have used before. Whatever was decoded
        // from that path last time is a different sound, so forget it.
        state.forget_sample(&path);
    }

    let sample = match state.load_sample(&path) {
        Ok(sample) => sample,
        Err(e) => {
            // Not a sound we can play, so do not leave it lying in the project folder.
            if state.project.lock().unwrap().sample_users(&imported.path) == 0 {
                let _ = folder::remove_sample(&dir, &imported.path);
            }
            return Err(e);
        }
    };

    let mut project = state.project.lock().unwrap();
    let id = project.free_track_id().unwrap_or(free);
    let track = Track::new(
        id,
        sample.name.clone(),
        Some(SampleRef {
            path: imported.path,
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

    project.tracks.push(track.clone());
    Ok(NewTrack {
        track,
        peaks: sample.peaks.clone(),
    })
}

/// Delete a track, and the sample with it if nothing else was using it.
#[tauri::command]
pub fn remove_track(id: u16, state: State<'_, Arc<AppState>>) {
    state.remember("tracks");
    let orphan = {
        let mut project = state.project.lock().unwrap();
        let removed = project.remove_track(id);
        state.send(Command::RemoveTrack { track: id });
        removed
            .and_then(|track| track.sample)
            .filter(|sample| project.sample_users(&sample.path) == 0)
    };
    if let Some(sample) = orphan {
        // The project folder holds exactly what the project uses, so this goes now rather
        // than at some tidy-up later.
        let _ = folder::remove_sample(&state.dir(), &sample.path);
    }
    state.touch();
}

#[tauri::command]
pub fn set_track_gain(id: u16, gain: f32, state: State<'_, Arc<AppState>>) {
    state.remember("gain");
    let mut project = state.project.lock().unwrap();
    if let Some(track) = project.track_mut(id) {
        track.gain = gain.clamp(0.0, 1.5);
        state.send(Command::SetTrackGain {
            track: id,
            gain: track.gain,
        });
    }
    state.touch();
}

#[tauri::command]
pub fn set_track_muted(id: u16, muted: bool, state: State<'_, Arc<AppState>>) {
    state.remember("mute");
    let mut project = state.project.lock().unwrap();
    if let Some(track) = project.track_mut(id) {
        track.muted = muted;
        state.send(Command::SetTrackMuted { track: id, muted });
    }
    state.touch();
}

#[tauri::command]
pub fn set_track_soloed(id: u16, soloed: bool, state: State<'_, Arc<AppState>>) {
    state.remember("solo");
    let mut project = state.project.lock().unwrap();
    if let Some(track) = project.track_mut(id) {
        track.soloed = soloed;
        state.send(Command::SetTrackSoloed { track: id, soloed });
    }
    state.touch();
}

/// Hear a track without waiting for its next step, at whatever pitch is asked for. Clicking
/// a row sends the sampler's own pitch; clicking a key in the piano roll sends that key's.
#[tauri::command]
pub fn audition(id: u16, pitch: Option<u8>, state: State<'_, Arc<AppState>>) {
    state.send(Command::Audition {
        track: id,
        pitch: pitch.unwrap_or(DEFAULT_PITCH),
        velocity: 110,
    });
}

/// A sampler instrument in this pattern rather than a one-shot: pitched, and its notes stop
/// when they end. Per pattern, so the same sound can be a rhythm in one and a melody in the
/// next.
#[tauri::command]
pub fn set_pattern_pitched(
    pattern: u16,
    track: u16,
    pitched: bool,
    state: State<'_, Arc<AppState>>,
) {
    state.remember("pitched");
    let mut project = state.project.lock().unwrap();
    if let Some(target) = project.pattern_mut(pattern) {
        target.set_pitched(track, pitched);
        state.send(Command::SetPatternPitched {
            pattern,
            track,
            pitched,
        });
    }
    drop(project);
    state.touch();
}

// --- patterns ---------------------------------------------------------------

/// Tick or untick a step in a pattern. Returns what the box actually is now.
#[tauri::command]
pub fn set_step(
    pattern: u16,
    track: u16,
    step: u32,
    on: bool,
    state: State<'_, Arc<AppState>>,
) -> bool {
    state.remember("boxes");
    let mut project = state.project.lock().unwrap();
    let Some(target) = project.pattern_mut(pattern) else {
        return false;
    };
    if step >= target.steps {
        return false;
    }
    let now_on = target.set_step(track, step, on);

    // The engine holds the same note, not a boolean, so the piano roll can send richer
    // ones later without either side changing.
    if now_on {
        let note = Note::step_note(step);
        state.send(Command::SetNote {
            pattern,
            track,
            note: EngineNote {
                step: note.step as u16,
                pitch: note.pitch,
                velocity: note.velocity,
                length: note.length as u16,
            },
        });
    } else {
        state.send(Command::ClearNote {
            pattern,
            track,
            step: step as u16,
            pitch: DEFAULT_PITCH,
        });
    }
    drop(project);
    state.touch();
    now_on
}

/// Add a note, or replace the one already at that step and pitch. What the piano roll draws
/// with, and how it moves velocity and length about afterwards.
///
/// A note drawn past the end of the pattern makes the pattern longer, so the answer says how
/// long the pattern is now as well as whether the note went in.
#[tauri::command]
pub fn set_note(
    pattern: u16,
    track: u16,
    at: At,
    velocity: u8,
    length: u32,
    state: State<'_, Arc<AppState>>,
) -> NotePut {
    state.remember("notes");
    let mut project = state.project.lock().unwrap();
    let Some(target) = project.pattern_mut(pattern) else {
        return NotePut::nowhere();
    };
    // A note past the end grows the pattern rather than being refused: drawing off the
    // right hand side of the roll is how a bar becomes two.
    let was = target.steps;
    let wanted = at.step.saturating_add(length.max(1));
    if wanted > was {
        target.set_steps(wanted);
    }
    let grew = target.steps != was;
    if at.step >= target.steps {
        return NotePut::nowhere();
    }
    let note = Note {
        step: at.step,
        pitch: at.pitch,
        velocity: velocity.clamp(1, 127),
        // A note cannot run off the end of its pattern.
        length: length.clamp(1, target.steps - at.step),
    };
    let fits = target.set_note(track, note);
    let steps = target.steps;
    if fits {
        state.send(Command::SetNote {
            pattern,
            track,
            note: EngineNote {
                step: note.step as u16,
                pitch: note.pitch,
                velocity: note.velocity,
                length: note.length as u16,
            },
        });
    }
    drop(project);
    if grew {
        state.send(Command::SetPatternSteps { pattern, steps });
    }
    state.touch();
    NotePut { fits, steps }
}

/// Take a note out.
#[tauri::command]
pub fn clear_note(pattern: u16, track: u16, at: At, state: State<'_, Arc<AppState>>) {
    state.remember("notes");
    let mut project = state.project.lock().unwrap();
    if let Some(target) = project.pattern_mut(pattern) {
        target.clear_note(track, at.step, at.pitch);
    }
    state.send(Command::ClearNote {
        pattern,
        track,
        step: at.step as u16,
        pitch: at.pitch,
    });
    drop(project);
    state.touch();
}

/// Move a note, keeping how long and how loud it is. One trip rather than two, so a dragged
/// note is never gone and back again.
#[tauri::command]
pub fn move_note(
    pattern: u16,
    track: u16,
    at: At,
    to: At,
    state: State<'_, Arc<AppState>>,
) -> bool {
    state.remember("notes");
    let mut project = state.project.lock().unwrap();
    let Some(target) = project.pattern_mut(pattern) else {
        return false;
    };
    let Some(note) = target.lane(track).and_then(|l| l.note(at.step, at.pitch)) else {
        return false;
    };
    if to.step >= target.steps {
        return false;
    }
    let moved = Note {
        step: to.step,
        pitch: to.pitch,
        length: note.length.clamp(1, target.steps - to.step),
        ..note
    };
    target.clear_note(track, at.step, at.pitch);
    let fits = target.set_note(track, moved);
    state.send(Command::ClearNote {
        pattern,
        track,
        step: at.step as u16,
        pitch: at.pitch,
    });
    if fits {
        state.send(Command::SetNote {
            pattern,
            track,
            note: EngineNote {
                step: moved.step as u16,
                pitch: moved.pitch,
                velocity: moved.velocity,
                length: moved.length as u16,
            },
        });
    }
    drop(project);
    state.touch();
    fits
}

#[tauri::command]
pub fn add_pattern(state: State<'_, Arc<AppState>>) -> Result<Arrangement, String> {
    state.remember("patterns");
    let id = state
        .project
        .lock()
        .unwrap()
        .add_pattern()
        .ok_or("that is as many patterns as there is room for")?;
    state.push_pattern(id);
    state.touch();
    Ok(arrangement(&state))
}

#[tauri::command]
pub fn duplicate_pattern(id: u16, state: State<'_, Arc<AppState>>) -> Result<Arrangement, String> {
    state.remember("patterns");
    let copy = state
        .project
        .lock()
        .unwrap()
        .duplicate_pattern(id)
        .ok_or("that is as many patterns as there is room for")?;
    state.push_pattern(copy);
    state.touch();
    Ok(arrangement(&state))
}

#[tauri::command]
pub fn remove_pattern(id: u16, state: State<'_, Arc<AppState>>) -> Result<Arrangement, String> {
    state.remember("patterns");
    {
        let mut project = state.project.lock().unwrap();
        if project.pattern(id).is_none() {
            // Nothing to delete means the front end is looking at something we threw away.
            // Say so rather than blaming the number of patterns.
            return Err(format!(
                "there is no pattern {id} to delete — reopen the project"
            ));
        }
        if !project.remove_pattern(id) {
            return Err("a song needs at least one pattern".into());
        }
    }
    state.send(Command::ClearPattern { pattern: id });
    state.push_song();
    state.touch();
    Ok(arrangement(&state))
}

/// Rename a pattern. Returns the name it ended up with: blank is not a name.
#[tauri::command]
pub fn rename_pattern(id: u16, name: String, state: State<'_, Arc<AppState>>) -> String {
    state.remember("name");
    let mut project = state.project.lock().unwrap();
    let fallback = project.next_pattern_name();
    let Some(pattern) = project.pattern_mut(id) else {
        return String::new();
    };
    let trimmed = name.trim();
    pattern.name = if trimmed.is_empty() {
        fallback
    } else {
        trimmed.chars().take(40).collect()
    };
    let named = pattern.name.clone();
    drop(project);
    state.touch();
    named
}

/// Give a pattern a colour, so its blocks stand out from the others in the song. `None`
/// puts it back to the one worked out from its place in the list.
#[tauri::command]
pub fn set_pattern_colour(id: u16, colour: Option<u8>, state: State<'_, Arc<AppState>>) -> bool {
    state.remember("colour");
    let mut project = state.project.lock().unwrap();
    let Some(pattern) = project.pattern_mut(id) else {
        return false;
    };
    pattern.colour = colour;
    drop(project);
    state.touch();
    true
}

/// Change how many boxes a pattern has. Returns the length it ended up with and the
/// arrangement, and drops any notes that no longer fit.
#[tauri::command]
pub fn set_pattern_steps(
    id: u16,
    steps: u32,
    state: State<'_, Arc<AppState>>,
) -> (u32, Arrangement) {
    state.remember("length");
    let steps = state.project.lock().unwrap().set_pattern_steps(id, steps);
    state.send(Command::SetPatternSteps { pattern: id, steps });
    // The trimmed notes have to go from the engine too, and there is no telling which ones
    // they were, so the whole pattern goes across again. The song is left alone: a block is
    // as long as it was put down, whatever its pattern does afterwards.
    state.push_pattern(id);
    state.touch();
    (steps, arrangement(&state))
}

/// Open a pattern in the editor: it is what plays, on a loop, until it is closed.
#[tauri::command]
pub fn open_pattern(id: u16, state: State<'_, Arc<AppState>>) {
    state.send(Command::SetActivePattern(id));
    state.send(Command::SetSongMode(false));
}

/// Close the editor and go back to the song.
#[tauri::command]
pub fn close_pattern(state: State<'_, Arc<AppState>>) {
    state.send(Command::SetSongMode(true));
}

// --- the song ---------------------------------------------------------------

/// Put a block of a pattern in the song, or take one out. Returns the song as it now is, so
/// the front end draws what is there rather than what it hoped for.
///
/// `step` is a step of the song, wherever the snap put it, and `length` is how much song the
/// block fills — zero for one play-through of the pattern. Taking one out wants the step it
/// starts at, which is what the front end hit tested to find it.
#[tauri::command]
pub fn place_pattern(
    pattern: u16,
    step: u32,
    length: u32,
    on: bool,
    state: State<'_, Arc<AppState>>,
) -> Vec<Placement> {
    state.remember("song");
    {
        let mut project = state.project.lock().unwrap();
        if on {
            project.place(pattern, step, length);
        } else {
            project.unplace(pattern, step);
        }
    }
    state.push_placement(pattern, step, on);
    state.touch();
    state.project.lock().unwrap().song.clone()
}

/// Slide a block along the song, keeping how long it is. `from` is anywhere along it.
#[tauri::command]
pub fn move_placement(
    pattern: u16,
    from: u32,
    to: u32,
    state: State<'_, Arc<AppState>>,
) -> Vec<Placement> {
    state.remember("song");
    let (was, moved) = {
        let mut project = state.project.lock().unwrap();
        let was = project.placement_at(pattern, from);
        (was, project.move_placement(pattern, from, to))
    };
    if let (true, Some(was)) = (moved, was) {
        state.push_placement(pattern, was.step, false);
        state.push_placement(pattern, to, true);
        state.touch();
    }
    state.project.lock().unwrap().song.clone()
}

/// Drag a block's edge: how much song it fills, one step at the least. A block longer than
/// its pattern repeats it.
#[tauri::command]
pub fn resize_placement(
    pattern: u16,
    step: u32,
    length: u32,
    state: State<'_, Arc<AppState>>,
) -> Vec<Placement> {
    state.remember("song");
    let placed = {
        let mut project = state.project.lock().unwrap();
        project
            .placement_at(pattern, step)
            .filter(|_| project.resize_placement(pattern, step, length))
    };
    if let Some(placed) = placed {
        state.push_placement(pattern, placed.step, true);
        state.touch();
    }
    state.project.lock().unwrap().song.clone()
}

/// Everything that starts in a bar, gone. Returns the song as it now is.
#[tauri::command]
pub fn clear_song_bar(bar: u32, state: State<'_, Arc<AppState>>) -> Vec<Placement> {
    state.remember("song");
    state.project.lock().unwrap().clear_bar(bar);
    state.push_song();
    state.touch();
    state.project.lock().unwrap().song.clone()
}

/// Drag the scrubber: play the song from this step.
#[tauri::command]
pub fn seek_song(step: u32, state: State<'_, Arc<AppState>>) {
    state.send(Command::SeekSong(step));
}

// --- undo and redo ----------------------------------------------------------

/// Step back, and hand the whole project over so the window can draw what is there now.
///
/// Whole projects rather than a description of what changed: the front end already knows how
/// to draw a project it has never seen — that is what opening one is — so undo is that, with
/// the view left where it was.
#[tauri::command]
pub fn undo(state: State<'_, Arc<AppState>>) -> Option<Startup> {
    stepped(&state, state.undo())
}

#[tauri::command]
pub fn redo(state: State<'_, Arc<AppState>>) -> Option<Startup> {
    stepped(&state, state.redo())
}

/// Undo from the menu bar. A menu item has nothing to return to, so this tells the front
/// end what the project is now by emitting the same event opening a project does.
pub fn stepped_by_menu(app: &AppHandle, back: bool) {
    let state = app_state(app);
    let moved = if back { state.undo() } else { state.redo() };
    if let Some(now) = stepped(&state, moved) {
        let _ = app.emit(STEPPED_EVENT, now);
    }
}

fn stepped(state: &AppState, moved: bool) -> Option<Startup> {
    if !moved {
        return None;
    }
    state.push_project();
    state.touch();
    // A sample that could not be found is the one thing a step back can get wrong — the
    // stash it would have come from is thrown away when a project is opened — so if the
    // engine had anything to complain about, it is said out loud rather than left as a
    // silent track.
    let message = state.complaint.lock().unwrap().take();
    Some(startup_payload(state, message))
}

// --- transport --------------------------------------------------------------

#[tauri::command]
pub fn set_bpm(bpm: f32, state: State<'_, Arc<AppState>>) -> f32 {
    state.remember("tempo");
    let mut project = state.project.lock().unwrap();
    project.bpm = bpm.clamp(40.0, 240.0);
    state.send(Command::SetBpm(project.bpm));
    let bpm = project.bpm;
    drop(project);
    state.touch();
    bpm
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
        patterns: playhead.patterns,
        voices: playhead.active_voices,
        peak: playhead.peak,
        stream_errors: state.stream_errors(),
        save_error: state.save_error(),
    }
}

// --- the project folder -----------------------------------------------------
//
// None of this is a command any more: opening and saving live in the File menu, which is a
// native menu built in `main.rs`. A menu event has no reply to return, so these tell the
// front end what happened by emitting an event at it instead.

/// A project the front end should draw from scratch: opened, or saved somewhere new.
pub const PROJECT_EVENT: &str = "project";
/// The project has been written out, and here is what it is called.
pub const SAVED_EVENT: &str = "saved";
/// A step back or forward. Like `PROJECT_EVENT`, except that the window stays where it is.
pub const STEPPED_EVENT: &str = "stepped";
/// Something went wrong, in words fit for the status line.
pub const TROUBLE_EVENT: &str = "trouble";

fn app_state(app: &AppHandle) -> Arc<AppState> {
    Arc::clone(app.state::<Arc<AppState>>().inner())
}

fn grumble(app: &AppHandle, trouble: String) {
    let _ = app.emit(TROUBLE_EVENT, trouble);
}

/// Write the project out now. It is written every second or so anyway; this is for the
/// person who wants to press the button.
pub fn save(app: &AppHandle) {
    let state = app_state(app);
    match state.save_now() {
        Ok(()) => {
            let _ = app.emit(SAVED_EVENT, state.name());
        }
        Err(e) => grumble(app, e),
    }
}

/// Copy the project, samples and all, to a folder of the user's choosing, and carry on
/// working in the new one.
pub fn save_as(app: &AppHandle) {
    let state = app_state(app);
    let name = format!("{}.{}", state.name(), folder::PROJECT_EXTENSION);
    let picked = app
        .dialog()
        .file()
        .set_title("Save the project")
        .set_file_name(name)
        .blocking_save_file();

    let Some(picked) = picked else { return };
    let mut target = match picked.into_path() {
        Ok(path) => path,
        Err(e) => return grumble(app, format!("that is not a folder we can save to: {e}")),
    };
    if target.extension().and_then(|e| e.to_str()) != Some(folder::PROJECT_EXTENSION) {
        // A project is a folder with a name that says what it is.
        let name = folder::name_of(&target);
        target.set_file_name(format!("{name}.{}", folder::PROJECT_EXTENSION));
    }

    let from = state.dir();
    if let Err(e) = state
        .save_now()
        .and_then(|()| folder::copy_folder(&from, &target))
    {
        return grumble(app, e);
    }
    state.set_dir(target);
    if let Err(e) = state.save_now() {
        return grumble(app, e);
    }
    let _ = app.emit(PROJECT_EVENT, startup_payload(&state, None));
}

/// Rename the project: the folder on disk gets the new name, since the folder's name *is*
/// the project's. Returns the name it ended up with.
#[tauri::command]
pub fn rename_project(name: String, state: State<'_, Arc<AppState>>) -> Result<String, String> {
    let from = state.dir();
    if name.trim() == state.name() {
        return Ok(state.name());
    }
    // Written out where it is before it moves, so a rename cannot lose the last edit.
    state.save_now()?;
    let to = folder::rename(&from, &name)?;
    state.set_dir(to);
    Ok(state.name())
}

/// Open another project folder. The one we are in is written out first, so nothing is lost
/// by wandering off.
pub fn open(app: &AppHandle) {
    let state = app_state(app);
    let mut dialog = app.dialog().file().set_title("Open a project");
    if let Some(above) = state.dir().parent() {
        dialog = dialog.set_directory(above);
    }
    let Some(picked) = dialog.blocking_pick_folder() else {
        return;
    };
    let target = match picked.into_path() {
        Ok(path) => path,
        Err(e) => return grumble(app, format!("that is not a folder we can open: {e}")),
    };
    if !folder::is_project(&target) {
        return grumble(
            app,
            format!(
                "{} is not a Weetbeats project — look for a folder ending in .{}",
                folder::name_of(&target),
                folder::PROJECT_EXTENSION
            ),
        );
    }

    // Write what we have out before reading anything, so opening the folder we are already
    // in cannot swap live work for an older copy of itself.
    if let Err(e) = state.save_now() {
        return grumble(app, e);
    }
    let opened = match folder::load(&target) {
        Ok(project) => project,
        Err(e) => return grumble(app, e),
    };

    state.send(Command::StopAll);
    *state.project.lock().unwrap() = opened;
    state.set_dir(target);
    // The steps that got the last project here mean nothing in this one, and the stash
    // they could have reached into is thrown away with them.
    state.forget_history();
    state.tidy_samples();
    state.push_project();
    let message = state.complaint.lock().unwrap().take();
    let _ = app.emit(PROJECT_EVENT, startup_payload(&state, message));
}
