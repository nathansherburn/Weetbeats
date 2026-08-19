//! What the app thread owns: the project, the sample cache, and the only handle to the
//! audio thread's command queue.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use rtrb::{Consumer, Producer};
use weetbeats_engine::command::{TrashBin, COMMAND_CAPACITY, TRASH_CAPACITY};
use weetbeats_engine::sample::decode_file;
use weetbeats_engine::{Command, Project, Sample, Shared, Trash};

use crate::audio::{self, AudioInfo};

pub struct AppState {
    pub project: Mutex<Project>,
    /// The only way to talk to the audio thread.
    tx: Mutex<Producer<Command>>,
    /// Samples handed back by the audio thread, waiting to be dropped here.
    trash: Mutex<Consumer<Trash>>,
    /// Decoded samples by path. Holding a reference here is also what makes it safe for
    /// the audio thread to let go of one: it is never the last owner.
    cache: Mutex<HashMap<PathBuf, Arc<Sample>>>,
    pub shared: Arc<Shared>,
    pub audio: AudioInfo,
    stream_errors: Arc<AtomicU32>,
}

impl AppState {
    /// Build the state and start the audio device.
    pub fn start() -> Result<Self, String> {
        let project = Project::default();
        let shared = Arc::new(Shared::new());
        let (tx, rx) = rtrb::RingBuffer::new(COMMAND_CAPACITY);
        let (trash_tx, trash_rx) = rtrb::RingBuffer::new(TRASH_CAPACITY);
        let stream_errors = Arc::new(AtomicU32::new(0));

        let audio = audio::spawn(
            rx,
            TrashBin::new(trash_tx, Arc::clone(&shared)),
            Arc::clone(&shared),
            Arc::clone(&stream_errors),
            project.bpm,
            project.pattern.steps,
        )?;

        let state = AppState {
            project: Mutex::new(project),
            tx: Mutex::new(tx),
            trash: Mutex::new(trash_rx),
            cache: Mutex::new(HashMap::new()),
            shared,
            audio,
            stream_errors,
        };
        state.send(Command::SetMasterGain(
            state.project.lock().unwrap().master_gain,
        ));
        Ok(state)
    }

    /// Push a command to the audio thread.
    ///
    /// The queue is a thousand deep and drained every callback, so it only fills up if the
    /// audio device has stopped. Dropping a command in that case is the right answer:
    /// blocking the UI would not bring the device back.
    pub fn send(&self, command: Command) {
        if let Ok(mut tx) = self.tx.lock() {
            let _ = tx.push(command);
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
}
