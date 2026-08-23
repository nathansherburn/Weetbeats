//! What the app thread owns: the project, the folder it lives in, the sample cache, and
//! the only handle to the audio thread's command queue.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rtrb::{Consumer, Producer};
use weetbeats_engine::command::{TrashBin, COMMAND_CAPACITY, TRASH_CAPACITY};
use weetbeats_engine::plugins::{Desk, Found, Param};
use weetbeats_engine::sample::decode_file;
use weetbeats_engine::{
    folder, Command, EngineNote, Pattern, Project, Sample, Shared, Trash, DEFAULT_STEPS,
    MAX_PATTERNS, MAX_TRACKS,
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

/// How many steps back you can go. A project is a few hundred notes and a list of samples,
/// so a hundred and twenty eight of them is a megabyte or two at the very worst.
const HISTORY_DEPTH: usize = 128;

/// Edits of the same kind closer together than this are one step. A drag across sixteen
/// boxes is one thing you did, and taking it back one box at a time would be maddening.
const COALESCE: Duration = Duration::from_millis(600);

/// What you can take back, and what you took back.
///
/// Whole projects rather than a list of changes. Every edit would otherwise need its own
/// opposite — and its opposite's opposite for redo — and the one that gets forgotten is the
/// one that loses your work. A project is small enough to copy, so it is copied.
#[derive(Default)]
struct History {
    past: Vec<Project>,
    future: Vec<Project>,
    /// What the last edit was called and when, for deciding whether the next one is part
    /// of the same gesture.
    last: Option<(&'static str, Instant)>,
}

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
    /// The plugin thread. Every CLAP instrument lives on it, because a plugin instance is
    /// `!Send` and CLAP says one thread owns it.
    desk: Desk,
    /// What a scan turned up last, so the picker does not have to open every `.clap` on the
    /// machine each time it is asked. Cleared by asking for a fresh scan.
    plugins: Mutex<Option<Vec<Found>>>,
    /// Which plugin the desk is holding for each track, as its file and its id.
    ///
    /// Only so that handing the whole project over again — which an undo does, once per step
    /// — does not tear down and rebuild every plugin. Loading Surge XT takes long enough that
    /// undoing a ticked box would stop the music.
    loaded: Mutex<HashMap<u16, (String, String)>>,
    /// Where the file picker opened last, so it does not send you back to your home
    /// folder every time.
    last_folder: Mutex<Option<PathBuf>>,
    /// Undo and redo.
    history: Mutex<History>,
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
            desk: Desk::start(audio.sample_rate as f64),
            plugins: Mutex::new(None),
            loaded: Mutex::new(HashMap::new()),
            last_folder: Mutex::new(None),
            history: Mutex::new(History::default()),
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
    ///
    /// Samples are dropped here. A plugin is not: letting go of the last piece of one
    /// destroys it, and CLAP says a plugin is destroyed on the thread that made it, so it
    /// goes back to the desk to be dropped there.
    pub fn take_out_the_trash(&self) {
        let Ok(mut trash) = self.trash.lock() else {
            return;
        };
        while let Ok(item) = trash.pop() {
            if let Trash::Plugin(slot) = item {
                self.desk.bin(slot);
            }
        }
    }

    pub fn stream_errors(&self) -> u32 {
        self.stream_errors.load(Ordering::Relaxed)
    }

    // --- plugins -----------------------------------------------------------

    /// Every CLAP plugin on the machine. Scanned once and remembered: opening every `.clap`
    /// takes a moment, and they do not appear while the app is running.
    pub fn plugins(&self, again: bool) -> Vec<Found> {
        let mut cached = self.plugins.lock().unwrap();
        if again {
            *cached = None;
        }
        cached.get_or_insert_with(|| self.desk.scan()).clone()
    }

    /// Put a plugin on a track: load it on the plugin thread, then hand the audio thread the
    /// half that belongs to it.
    ///
    /// The track's own settings for that plugin come out of the project folder, so opening a
    /// project puts the patch back the way it was.
    pub fn load_plugin(&self, track: u16, path: &str, id: &str) -> Result<String, String> {
        let state = {
            let project = self.project.lock().unwrap();
            project
                .track(track)
                .and_then(|t| t.plugin.as_ref())
                .and_then(|p| p.state.as_ref())
                .and_then(|relative| folder::load_plugin_state(&self.dir(), relative))
        };
        let loaded = self.desk.load(track, Path::new(path), id, state)?;
        self.send(Command::SetTrackPlugin {
            track,
            slot: Some(loaded.slot),
        });
        self.loaded
            .lock()
            .unwrap()
            .insert(track, (path.to_string(), id.to_string()));
        Ok(loaded.name)
    }

    /// Take it off, both halves.
    pub fn unload_plugin(&self, track: u16) {
        self.send(Command::SetTrackPlugin { track, slot: None });
        self.desk.unload(track);
        self.loaded.lock().unwrap().remove(&track);
    }

    pub fn plugin_params(&self, track: u16) -> Vec<Param> {
        self.desk.params(track)
    }

    /// Let any plugin that asked for it have its turn on the main thread. Called from the
    /// playhead poll, which runs whether or not anything is playing.
    pub fn tick_plugins(&self) {
        self.desk.tick();
    }

    /// Ask every plugin what it is set to and write it into the project folder.
    ///
    /// Done as part of saving rather than on every knob turn: a plugin's state is the whole
    /// patch, which for Surge XT is a good few kilobytes, and it is only ever read again when
    /// the project is opened.
    fn save_plugin_states(&self) {
        let tracks: Vec<(u16, String)> = {
            let mut project = self.project.lock().unwrap();
            let mut wanted = Vec::new();
            for track in &mut project.tracks {
                let id = track.id;
                if let Some(plugin) = track.plugin.as_mut() {
                    let relative = plugin
                        .state
                        .get_or_insert_with(|| folder::plugin_state_path(id))
                        .clone();
                    wanted.push((id, relative));
                }
            }
            wanted
        };
        let dir = self.dir();
        for (track, relative) in tracks {
            if let Some(blob) = self.desk.save_state(track) {
                let _ = folder::save_plugin_state(&dir, &relative, &blob);
            }
        }
    }

    /// Hand the audio thread the whole project: every track, every pattern, the song.
    ///
    /// Used at startup and when a project is opened. Tracks go first, because claiming a
    /// track slot clears whatever notes the last thing in that slot had.
    pub fn push_project(&self) {
        let project = self.project.lock().unwrap();
        let dir = self.dir();

        self.send(Command::SetPlaying(false));

        // A plugin that is already on the track it is going back onto stays where it is.
        // Everything else about the track is sent again, but the instance is not rebuilt: an
        // undo hands the whole project over, and reloading Surge XT once per undone box is
        // not a thing anybody would sit through.
        let staying: Vec<u16> = {
            let loaded = self.loaded.lock().unwrap();
            project
                .tracks
                .iter()
                .filter(|track| {
                    track.plugin.as_ref().is_some_and(|plugin| {
                        loaded.get(&track.id) == Some(&(plugin.path.clone(), plugin.id.clone()))
                    })
                })
                .map(|track| track.id)
                .collect()
        };
        for track in 0..MAX_TRACKS as u16 {
            if !staying.contains(&track) {
                self.send(Command::RemoveTrack { track });
                // And the desk lets go of anything it was holding for a slot that is not
                // keeping it, so a plugin taken off by an undo really does go.
                if self.loaded.lock().unwrap().remove(&track).is_some() {
                    self.desk.unload(track);
                }
            }
        }
        for pattern in 0..MAX_PATTERNS as u16 {
            self.send(Command::ClearPattern { pattern });
        }

        self.send(Command::SetBpm(project.bpm));
        self.send(Command::SetMasterGain(project.master_gain));

        let mut trouble: Vec<String> = Vec::new();
        for track in &project.tracks {
            self.send(Command::AddTrack { track: track.id });
            // Before the sound, so a track can never be heard for a moment as its unshaped
            // self while a project is still going across.
            self.send(Command::SetTrackVoicing {
                track: track.id,
                voicing: track.voicing,
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
            if let Some(reference) = &track.plugin {
                if staying.contains(&track.id) {
                    // Already there, still playing, keeping its patch. Nothing to do.
                    continue;
                }
                // Loading Surge XT takes a moment, and a project with several of them takes
                // several. Nothing else can start until they are in, because the notes have
                // to have somewhere to go.
                //
                // A plugin that is not on this machine leaves the track silent and says so,
                // the same as a sample that would not decode: the part you wrote is still
                // there, and installing the plugin brings it back.
                let state = reference
                    .state
                    .as_ref()
                    .and_then(|relative| folder::load_plugin_state(&dir, relative));
                match self
                    .desk
                    .load(track.id, Path::new(&reference.path), &reference.id, state)
                {
                    Ok(loaded) => {
                        self.send(Command::SetTrackPlugin {
                            track: track.id,
                            slot: Some(loaded.slot),
                        });
                        self.loaded
                            .lock()
                            .unwrap()
                            .insert(track.id, (reference.path.clone(), reference.id.clone()));
                    }
                    Err(e) => trouble.push(format!("{}: {e}", reference.name)),
                }
            }
        }

        for pattern in &project.patterns {
            self.send(Command::SetPatternSteps {
                pattern: pattern.id,
                steps: pattern.steps,
            });
            self.push_mix(pattern);
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
                length: placement.length,
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
        self.push_mix(pattern);
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

    /// One pattern's mixer: how loud each track is in it, what is muted, what is soloed, and
    /// which tracks are instruments. Only the tracks that differ from the default are in the
    /// project, and the audio thread starts every pattern at the default, so only those need
    /// sending.
    fn push_mix(&self, pattern: &Pattern) {
        for mix in &pattern.mix {
            self.send(Command::SetPatternGain {
                pattern: pattern.id,
                track: mix.track,
                gain: mix.gain,
            });
            self.send(Command::SetPatternMuted {
                pattern: pattern.id,
                track: mix.track,
                muted: mix.muted,
            });
            self.send(Command::SetPatternSoloed {
                pattern: pattern.id,
                track: mix.track,
                soloed: mix.soloed,
            });
            self.send(Command::SetPatternPitched {
                pattern: pattern.id,
                track: mix.track,
                pitched: mix.pitched,
            });
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
                length: placement.length,
            });
        }
        self.send(Command::SetSongLen(project.song_steps()));
    }

    /// One block, plus how long the song is now. What painting the song sends, so a drag
    /// across it is two commands a block rather than the whole song each time.
    pub fn push_placement(&self, pattern: u16, step: u32, on: bool) {
        let project = self.project.lock().unwrap();
        self.send(match project.placement_at(pattern, step).filter(|_| on) {
            Some(placed) => Command::PlacePattern {
                pattern,
                step: placed.step,
                length: placed.length,
            },
            None => Command::UnplacePattern { pattern, step },
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
        // And the stash with it: a window that has just opened this project has nothing to
        // undo, so nothing in there can ever be wanted again.
        let _ = folder::clear_stash(&dir);
    }

    // --- undo and redo -----------------------------------------------------

    /// Keep the project as it is now, before an edit changes it.
    ///
    /// Called at the top of everything that edits, *before* the project is locked: the
    /// history lock is only ever taken on its own, so the two can never wait on each other.
    /// `what` names the kind of edit, so a run of the same kind in quick succession — a drag
    /// — is one step rather than one step per box.
    pub fn remember(&self, what: &'static str) {
        let mut history = self.history.lock().unwrap();
        let now = Instant::now();
        let carrying_on = history
            .last
            .is_some_and(|(name, at)| name == what && now.duration_since(at) < COALESCE);
        history.last = Some((what, now));
        if carrying_on {
            return;
        }
        // Anything undone is no longer ahead of us: this is a different future now.
        history.future.clear();
        let snapshot = self.project.lock().unwrap().clone();
        history.past.push(snapshot);
        if history.past.len() > HISTORY_DEPTH {
            history.past.remove(0);
        }
    }

    /// Step back. `false` when there is nowhere to step back to.
    pub fn undo(&self) -> bool {
        self.step(true)
    }

    /// And forward again.
    pub fn redo(&self) -> bool {
        self.step(false)
    }

    fn step(&self, back: bool) -> bool {
        let mut history = self.history.lock().unwrap();
        let taken = if back {
            history.past.pop()
        } else {
            history.future.pop()
        };
        let Some(taken) = taken else {
            return false;
        };
        let left_behind = std::mem::replace(&mut *self.project.lock().unwrap(), taken);
        if back {
            history.future.push(left_behind);
        } else {
            history.past.push(left_behind);
        }
        // The next edit starts a new step, whatever it is: nothing is being carried on from
        // before an undo.
        history.last = None;
        drop(history);
        // A sample the undone step deleted comes back out of the stash, and one it added
        // goes into it, so the folder describes the project we now have.
        let dir = self.dir();
        let _ = folder::reconcile_samples(&dir, &self.project.lock().unwrap());
        true
    }

    /// Nothing to take back. For opening a different project, where the steps that got the
    /// last one here would make no sense.
    pub fn forget_history(&self) {
        let mut history = self.history.lock().unwrap();
        history.past.clear();
        history.future.clear();
        history.last = None;
    }

    /// Mark the project as needing writing. The saver picks it up within a moment.
    pub fn touch(&self) {
        self.dirty.store(true, Ordering::Relaxed);
    }

    /// Write the project out now, whether or not anything changed.
    pub fn save_now(&self) -> Result<(), String> {
        // The plugins first: each one's settings are a file of its own beside the project,
        // and `project.json` has to name a file that is already there.
        self.save_plugin_states();
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
        // Named by its file, not its path: the status line has one line, and a project
        // folder on the Desktop uses most of it up before the reason gets a look in.
        let sample = Arc::new(decode_file(path).map_err(|e| {
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("that sound");
            format!("{name} could not be read: {e}")
        })?);
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
