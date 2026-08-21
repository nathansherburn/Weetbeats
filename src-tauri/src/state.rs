//! What the app thread owns: the project, the folder it lives in, the sample cache, and
//! the only handle to the audio thread's command queue.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rtrb::{Consumer, Producer};
use weetbeats_engine::command::{TrashBin, COMMAND_CAPACITY, TRASH_CAPACITY};
use weetbeats_engine::sample::decode_file;
use weetbeats_engine::{
    folder, Command, EngineNote, Project, Sample, Shared, Trash, DEFAULT_STEPS, MAX_PATTERNS,
    MAX_TRACKS,
};

use crate::audio;

/// Must match the identifier in `tauri.conf.json`: it names the folder the app keeps its
/// own things in.
#[cfg(target_os = "macos")]
const BUNDLE_ID: &str = "com.weetbeats.desktop";

/// How often the project is written out, in milliseconds. Every edit marks it dirty and
/// this picks the work up, so a drag across sixteen boxes is one write, not sixteen.
const SAVE_EVERY: u64 = 800;

/// How long the app thread will wait for room in the command queue before giving up on a
/// command. Only ever reached when the audio device has stopped answering: while a project
/// is going across, room appears within a callback.
const QUEUE_WAIT: Duration = Duration::from_millis(50);

pub struct AppState {
    pub project: Mutex<Project>,
    /// The folder the project lives in. Samples are copied in here as they are added.
    dir: Mutex<PathBuf>,
    /// Set by every edit, cleared by the saver.
    dirty: AtomicBool,
    /// What went wrong the last time we tried to write the project out, if anything. Shown
    /// in the UI, because a save that quietly fails is how work disappears.
    save_error: Mutex<Option<String>>,
    /// Something to tell the user at startup: a project that would not open, a sample that
    /// would not decode.
    pub complaint: Mutex<Option<String>>,
    /// The only way to talk to the audio thread.
    tx: Mutex<Producer<Command>>,
    /// Set when a command had to be dropped. While it is set nothing waits for room in the
    /// queue: a stopped audio device would otherwise turn one lost command into a frozen
    /// window, once per command, for as long as the project takes to send.
    queue_stuck: AtomicBool,
    /// Samples handed back by the audio thread, waiting to be dropped here.
    trash: Mutex<Consumer<Trash>>,
    /// Decoded samples by path. Holding a reference here is also what makes it safe for
    /// the audio thread to let go of one: it is never the last owner.
    cache: Mutex<HashMap<PathBuf, Arc<Sample>>>,
    pub shared: Arc<Shared>,
    stream_errors: Arc<AtomicU32>,
    /// Where the file picker opened last, so it does not send you back to your home
    /// folder every time.
    last_folder: Mutex<Option<PathBuf>>,
}

impl AppState {
    /// Build the state, open the project we had last time, and start the audio device.
    pub fn start() -> Result<Self, String> {
        let (dir, project, complaint) = open_last_project();
        let shared = Arc::new(Shared::new());
        let (tx, rx) = rtrb::RingBuffer::new(COMMAND_CAPACITY);
        let (trash_tx, trash_rx) = rtrb::RingBuffer::new(TRASH_CAPACITY);
        let stream_errors = Arc::new(AtomicU32::new(0));

        // What the device turned out to be is worth knowing when something is wrong with it,
        // and nowhere near worth a corner of the window.
        let audio = audio::spawn(
            rx,
            TrashBin::new(trash_tx, Arc::clone(&shared)),
            Arc::clone(&shared),
            Arc::clone(&stream_errors),
            project.bpm,
            project
                .patterns
                .first()
                .map(|p| p.steps)
                .unwrap_or(DEFAULT_STEPS),
        )?;
        eprintln!(
            "Weetbeats: {} at {}Hz, {} channels, {}",
            audio.device, audio.sample_rate, audio.channels, audio.format
        );

        let state = AppState {
            project: Mutex::new(project),
            dir: Mutex::new(dir),
            dirty: AtomicBool::new(false),
            save_error: Mutex::new(None),
            complaint: Mutex::new(complaint),
            tx: Mutex::new(tx),
            queue_stuck: AtomicBool::new(false),
            trash: Mutex::new(trash_rx),
            cache: Mutex::new(HashMap::new()),
            shared,
            stream_errors,
            last_folder: Mutex::new(None),
        };
        state.remember_where_we_were();
        state.tidy_samples();
        state.push_project();
        Ok(state)
    }

    // --- talking to the audio thread ---------------------------------------

    /// Push a command to the audio thread.
    ///
    /// The queue is thousands deep and drained every callback, so a full one means either
    /// the audio device has stopped or a whole project is going across at once. Waiting a
    /// moment covers the second, where room appears as soon as the next callback runs.
    /// Giving up covers the first, because blocking the UI would not bring the device back
    /// — and once one command has been given up on, the rest do not wait at all.
    pub fn send(&self, command: Command) {
        let Ok(mut tx) = self.tx.lock() else { return };
        let mut command = command;
        let patient = !self.queue_stuck.load(Ordering::Relaxed);
        let deadline = std::time::Instant::now() + QUEUE_WAIT;
        loop {
            match tx.push(command) {
                Ok(()) => {
                    self.queue_stuck.store(false, Ordering::Relaxed);
                    return;
                }
                Err(rtrb::PushError::Full(back)) => {
                    if !patient || std::time::Instant::now() >= deadline {
                        self.queue_stuck.store(true, Ordering::Relaxed);
                        return;
                    }
                    command = back;
                    std::thread::sleep(Duration::from_micros(200));
                }
            }
        }
    }

    /// Drop anything the audio thread handed back. Called from the playhead poll, so it
    /// happens about sixty times a second while the app is open.
    pub fn take_out_the_trash(&self) {
        if let Ok(mut trash) = self.trash.lock() {
            while trash.pop().is_ok() {}
        }
    }

    pub fn stream_errors(&self) -> u32 {
        self.stream_errors.load(Ordering::Relaxed)
    }

    /// Hand the audio thread the whole project: every track, every pattern, the song.
    ///
    /// Used at startup and when a project is opened. Tracks go first, because claiming a
    /// track slot clears whatever notes the last thing in that slot had.
    pub fn push_project(&self) {
        let project = self.project.lock().unwrap();
        let dir = self.dir();

        self.send(Command::SetPlaying(false));
        for track in 0..MAX_TRACKS as u16 {
            self.send(Command::RemoveTrack { track });
        }
        for pattern in 0..MAX_PATTERNS as u16 {
            self.send(Command::ClearPattern { pattern });
        }

        self.send(Command::SetBpm(project.bpm));
        self.send(Command::SetMasterGain(project.master_gain));

        let mut trouble: Vec<String> = Vec::new();
        for track in &project.tracks {
            self.send(Command::AddTrack {
                track: track.id,
                gain: track.gain,
            });
            self.send(Command::SetTrackMuted {
                track: track.id,
                muted: track.muted,
            });
            self.send(Command::SetTrackSoloed {
                track: track.id,
                soloed: track.soloed,
            });
            if let Some(reference) = &track.sample {
                match folder::resolve(&dir, &reference.path)
                    .and_then(|path| self.load_sample(&path))
                {
                    Ok(sample) => self.send(Command::SetTrackSample {
                        track: track.id,
                        sample: Some(sample),
                    }),
                    // The track stays, silent, with its notes: losing a sound is annoying,
                    // losing the beat you wrote with it is worse.
                    Err(e) => trouble.push(e),
                }
            }
        }

        for pattern in &project.patterns {
            self.send(Command::SetPatternSteps {
                pattern: pattern.id,
                steps: pattern.steps,
            });
            for lane in &pattern.lanes {
                for note in &lane.notes {
                    self.send(Command::SetNote {
                        pattern: pattern.id,
                        track: lane.track,
                        note: EngineNote {
                            step: note.step as u16,
                            pitch: note.pitch,
                            velocity: note.velocity,
                            length: note.length as u16,
                        },
                    });
                }
            }
        }

        self.send(Command::ClearSong);
        for placement in &project.song {
            self.send(Command::PlacePattern {
                pattern: placement.pattern,
                step: placement.step,
            });
        }
        self.send(Command::SetSongLen(project.song_steps()));
        self.send(Command::SetActivePattern(
            project.patterns.first().map(|p| p.id).unwrap_or(0),
        ));
        self.send(Command::SetSongMode(!project.song.is_empty()));

        if let Some(first) = trouble.first() {
            let more = if trouble.len() > 1 {
                format!(" (and {} more)", trouble.len() - 1)
            } else {
                String::new()
            };
            *self.complaint.lock().unwrap() = Some(format!("{first}{more}"));
        }
    }

    /// Tell the audio thread everything about one pattern, from nothing. For a pattern
    /// that has just been made, or copied from another.
    pub fn push_pattern(&self, id: u16) {
        let project = self.project.lock().unwrap();
        let Some(pattern) = project.pattern(id) else {
            return;
        };
        self.send(Command::ClearPattern { pattern: id });
        self.send(Command::SetPatternSteps {
            pattern: id,
            steps: pattern.steps,
        });
        for lane in &pattern.lanes {
            for note in &lane.notes {
                self.send(Command::SetNote {
                    pattern: id,
                    track: lane.track,
                    note: EngineNote {
                        step: note.step as u16,
                        pitch: note.pitch,
                        velocity: note.velocity,
                        length: note.length as u16,
                    },
                });
            }
        }
    }

    /// Tell the audio thread the song again, from nothing. Cheap: a song is a few hundred
    /// placements at most, and it means the front end never has to describe an edit, only
    /// the result.
    pub fn push_song(&self) {
        let project = self.project.lock().unwrap();
        self.send(Command::ClearSong);
        for placement in &project.song {
            self.send(Command::PlacePattern {
                pattern: placement.pattern,
                step: placement.step,
            });
        }
        self.send(Command::SetSongLen(project.song_steps()));
    }

    /// One placement, plus how long the song is now. What painting the song sends, so a drag
    /// across it is two commands a placement rather than the whole song each time.
    pub fn push_placement(&self, pattern: u16, step: u32, on: bool) {
        let project = self.project.lock().unwrap();
        self.send(if on {
            Command::PlacePattern { pattern, step }
        } else {
            Command::UnplacePattern { pattern, step }
        });
        self.send(Command::SetSongLen(project.song_steps()));
    }

    // --- the project folder ------------------------------------------------

    pub fn dir(&self) -> PathBuf {
        self.dir.lock().unwrap().clone()
    }

    /// Move to a different folder and remember it for next time.
    pub fn set_dir(&self, dir: PathBuf) {
        *self.dir.lock().unwrap() = dir;
        self.remember_where_we_were();
    }

    pub fn name(&self) -> String {
        folder::name_of(&self.dir())
    }

    /// Delete samples in the folder that no track refers to.
    ///
    /// Nothing normally leaves one behind — a sample goes when its track does — so this is
    /// for the folder that was interrupted halfway through being added to, and for one
    /// someone has been editing by hand. Only ever runs on a project we have just opened,
    /// where the track list is everything the project has to say.
    pub fn tidy_samples(&self) {
        let dir = self.dir();
        let project = self.project.lock().unwrap();
        let _ = folder::forget_unused_samples(&dir, &project);
    }

    /// Mark the project as needing writing. The saver picks it up within a moment.
    pub fn touch(&self) {
        self.dirty.store(true, Ordering::Relaxed);
    }

    /// Write the project out now, whether or not anything changed.
    pub fn save_now(&self) -> Result<(), String> {
        let dir = self.dir();
        let result = {
            let project = self.project.lock().unwrap();
            folder::save(&dir, &project)
        };
        self.dirty.store(result.is_err(), Ordering::Relaxed);
        *self.save_error.lock().unwrap() = result.as_ref().err().cloned();
        result
    }

    fn save_if_dirty(&self) {
        if self.dirty.swap(false, Ordering::Relaxed) {
            let _ = self.save_now();
        }
    }

    pub fn save_error(&self) -> Option<String> {
        self.save_error.lock().unwrap().clone()
    }

    /// Note the folder we are working in, so the next launch opens the same project.
    fn remember_where_we_were(&self) {
        let dir = self.dir();
        let _ = std::fs::create_dir_all(data_dir());
        let _ = std::fs::write(pointer_file(), dir.to_string_lossy().as_bytes());
    }

    pub fn last_folder(&self) -> Option<PathBuf> {
        self.last_folder.lock().ok()?.clone()
    }

    pub fn remember_folder(&self, folder: &Path) {
        if let Ok(mut last) = self.last_folder.lock() {
            *last = Some(folder.to_path_buf());
        }
    }

    // --- samples -----------------------------------------------------------

    /// Decode a sample, or hand back the one already in the cache.
    ///
    /// Blocking and allocating on purpose. Call it from a blocking task, never from the
    /// audio thread.
    pub fn load_sample(&self, path: &Path) -> Result<Arc<Sample>, String> {
        if let Some(sample) = self.cache.lock().unwrap().get(path) {
            return Ok(Arc::clone(sample));
        }
        let sample = Arc::new(
            decode_file(path).map_err(|e| format!("could not read {}: {e}", path.display()))?,
        );
        self.cache
            .lock()
            .unwrap()
            .insert(path.to_path_buf(), Arc::clone(&sample));
        Ok(sample)
    }

    /// Forget what we decoded from a path, because the file there is not that any more.
    ///
    /// The cache is keyed by path, and a project folder reuses names: without this, adding
    /// a different kick that lands on a name a deleted one had would play the old sound.
    pub fn forget_sample(&self, path: &Path) {
        self.cache.lock().unwrap().remove(path);
    }
}

/// Write the project out every so often, on a thread of its own.
///
/// Not on every edit: painting a bar of hats is sixteen edits in a second and one write is
/// plenty. Not only on quit either — a crash should cost you the last half second, not the
/// afternoon.
pub fn spawn_saver(state: Arc<AppState>) {
    std::thread::Builder::new()
        .name("weetbeats-saver".into())
        .spawn(move || loop {
            std::thread::sleep(Duration::from_millis(SAVE_EVERY));
            state.save_if_dirty();
        })
        .expect("could not start the saver thread");
}

// --- where things live ------------------------------------------------------

/// Where the app keeps its own things: new projects, and a note of which one was open.
#[cfg(target_os = "macos")]
fn data_dir() -> PathBuf {
    home().join("Library/Application Support").join(BUNDLE_ID)
}

/// Only so the project code can be run and tested off a Mac. macOS is the target.
#[cfg(not(target_os = "macos"))]
fn data_dir() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".local/share"))
        .join("weetbeats")
}

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Holds the path of the project that was open, so the app comes back to it.
fn pointer_file() -> PathBuf {
    data_dir().join("last-project.txt")
}

/// The project to open at startup: the one that was open last, or a new one.
///
/// A project that will not load is left alone rather than started over: we move to a fresh
/// folder and say what happened, so the broken one is still there to be looked at.
fn open_last_project() -> (PathBuf, Project, Option<String>) {
    let remembered = std::fs::read_to_string(pointer_file())
        .ok()
        .map(|text| PathBuf::from(text.trim()))
        .filter(|dir| folder::is_project(dir));

    match remembered {
        Some(dir) => match folder::load(&dir) {
            Ok(project) => (dir, project, None),
            Err(e) => (fresh_folder(), Project::default(), Some(e)),
        },
        None => (fresh_folder(), Project::default(), None),
    }
}

/// A folder for a project that has never been saved anywhere: `Untitled.beat` in the app's
/// own data folder, or `Untitled 2.beat` if that one is taken.
fn fresh_folder() -> PathBuf {
    let data = data_dir();
    let first = data.join(format!("Untitled.{}", folder::PROJECT_EXTENSION));
    if !folder::is_project(&first) {
        return first;
    }
    for n in 2..1000 {
        let next = data.join(format!("Untitled {n}.{}", folder::PROJECT_EXTENSION));
        if !folder::is_project(&next) {
            return next;
        }
    }
    first
}
