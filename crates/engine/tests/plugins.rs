//! Hosting a real CLAP plugin.
//!
//! Everything here goes through an actual dynamic library — `tools/test-clap`, a synth the
//! size of a postage stamp — because that is the only way any of it can be checked. The
//! thread rules, the port declarations, the note events, the parameter events and the state
//! blob are agreements with somebody else's binary; a mock on our side of the line would
//! only ever agree with itself.
//!
//! The plugin plays one sine wave a note, at the pitch of the note, at a level its one
//! parameter sets. So "the note arrived" is "there is sound", "the note stopped" is "there is
//! not", and "the parameter arrived" is "there is more or less of it".

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use weetbeats_engine::command::{TrashBin, COMMAND_CAPACITY, TRASH_CAPACITY};
use weetbeats_engine::plugins::{Desk, Studio};
use weetbeats_engine::{Command, Engine, EngineNote, Shared, Trash};

const RATE: u32 = 48_000;
/// The plugin's own id, from `tools/test-clap`.
const TEST_PLUGIN: &str = "com.weetbeats.test-tone";

/// The `.clap` we are hosting, put somewhere a scan would look.
///
/// Cargo builds the fixture as a plain dynamic library beside the test binary; a scan only
/// looks at files called `.clap`, so it is copied to one. The folder it goes in is handed to
/// the scanner through `CLAP_PATH`, which is the same way somebody with plugins in an unusual
/// place would tell a host about them.
///
/// Set up exactly once for the whole test binary. Copying the file again while another test
/// has it loaded rewrites the pages that test is executing out of, which is a bus error and
/// not a helpful one; and the folder is left behind at the end for the same reason, since
/// the plugins are still mapped when the last test finishes.
struct Fixture {
    dir: PathBuf,
    file: PathBuf,
}

fn fixture() -> &'static Fixture {
    static ONCE: OnceLock<Fixture> = OnceLock::new();
    ONCE.get_or_init(|| {
        build_fixture();
        let built = built_library().unwrap_or_else(|| {
            panic!(
                "the test plugin was not built. Try `cargo build -p weetbeats-test-clap` and \
                 see what it says."
            )
        });
        let dir = std::env::temp_dir().join(format!("weetbeats-clap-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("could not make a folder for the test plugin");
        let file = dir.join("weetbeats-test-tone.clap");
        std::fs::copy(&built, &file).expect("could not put the test plugin where a scan looks");
        Fixture { dir, file }
    })
}

/// Build the fixture, here, before anything looks for it.
///
/// It is a dev dependency of this crate, which gets it compiled and type checked — but a
/// dev dependency is linked as a library, and what a test needs is the *dynamic* library
/// cargo also builds from it. Nothing makes cargo refresh that before running the tests, so
/// editing the plugin and running `cargo test` would quietly go on testing the last one
/// built. A test that can silently check the wrong binary is worse than no test, so this
/// builds it and says so if it cannot.
///
/// Nested cargo: the outer one has finished building and released its lock by the time a test
/// runs, so this is safe, if a second or two of nothing when everything is already up to date.
fn build_fixture() {
    let manifest =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tools/test-clap/Cargo.toml");
    let built = std::process::Command::new(env!("CARGO"))
        .args(["build", "--quiet", "--manifest-path"])
        .arg(&manifest)
        .output();
    match built {
        Ok(done) if done.status.success() => {}
        Ok(done) => panic!(
            "the test plugin would not build:\n{}",
            String::from_utf8_lossy(&done.stderr)
        ),
        Err(e) => panic!("could not build the test plugin: {e}"),
    }
}

/// Where cargo put the fixture: beside the test binary, one folder up out of `deps`.
fn built_library() -> Option<PathBuf> {
    let name = if cfg!(target_os = "windows") {
        "weetbeats_test_clap.dll"
    } else if cfg!(target_os = "macos") {
        "libweetbeats_test_clap.dylib"
    } else {
        "libweetbeats_test_clap.so"
    };
    let here = std::env::current_exe().ok()?;
    let target = here.parent()?.parent()?;
    let built = target.join(name);
    built.exists().then_some(built)
}

/// The one plugin thread the whole test binary shares, with the fixture's folder on its
/// search path.
///
/// One of them, because the tests run in parallel in one process and a desk is a thread that
/// owns plugin instances: two of them fighting over the same track would be testing the
/// harness rather than the host. Each test below works on a track of its own instead.
fn desk() -> &'static Desk {
    static ONCE: OnceLock<Desk> = OnceLock::new();
    ONCE.get_or_init(|| {
        // SAFETY: setting an environment variable is only unsound while another thread is
        // reading one. This runs inside a `OnceLock`, before the desk that reads it exists.
        unsafe { std::env::set_var("CLAP_PATH", &fixture().dir) };
        Desk::start(RATE as f64)
    })
}

struct Rig {
    engine: Box<Engine>,
    tx: rtrb::Producer<Command>,
    #[allow(dead_code)]
    trash_rx: rtrb::Consumer<Trash>,
}

impl Rig {
    fn new(bpm: f32, steps: u32) -> Self {
        let shared = Arc::new(Shared::new());
        let (tx, rx) = rtrb::RingBuffer::new(COMMAND_CAPACITY);
        let (trash_tx, trash_rx) = rtrb::RingBuffer::new(TRASH_CAPACITY);
        let engine = Engine::new(
            RATE,
            bpm,
            steps,
            Arc::clone(&shared),
            rx,
            TrashBin::new(trash_tx, Arc::clone(&shared)),
        );
        Rig {
            engine,
            tx,
            trash_rx,
        }
    }

    fn send(&mut self, command: Command) {
        self.tx.push(command).expect("command queue full");
    }

    fn render(&mut self, frames: usize) -> Vec<f32> {
        let mut out = vec![0.0f32; frames * 2];
        self.engine.render(&mut out, 2);
        out
    }

    /// Render in awkward chunks, the way a real device would.
    fn render_chunked(&mut self, frames: usize, chunk: usize) -> Vec<f32> {
        let mut out = Vec::with_capacity(frames * 2);
        let mut left = frames;
        while left > 0 {
            let n = chunk.min(left);
            out.extend_from_slice(&self.render(n));
            left -= n;
        }
        out
    }
}

fn peak(out: &[f32]) -> f32 {
    out.iter().fold(0.0f32, |a, b| a.max(b.abs()))
}

/// The loudest thing between two frames, so a stretch of the output can be looked at on its
/// own — before a note, during it, after it.
fn peak_between(out: &[f32], from: usize, to: usize) -> f32 {
    let to = to.min(out.len() / 2);
    if from >= to {
        return 0.0;
    }
    peak(&out[from * 2..to * 2])
}

fn note(step: u16, pitch: u8, length: u16) -> EngineNote {
    EngineNote {
        step,
        pitch,
        velocity: 100,
        length,
    }
}

/// A track playing the test plugin, with the plugin thread it came from kept alive: the
/// plugin instance lives on that thread, and dropping the desk would take it away.
fn plugin_track(rig: &mut Rig, track: u16) {
    let loaded = desk()
        .load(track, &fixture().file, TEST_PLUGIN, None)
        .expect("the test plugin would not load");
    rig.send(Command::AddTrack { track });
    rig.send(Command::SetTrackPlugin {
        track,
        slot: Some(loaded.slot),
    });
    // Notes on a plugin track mean pitch and length, which is what being an instrument means.
    rig.send(Command::SetPatternPitched {
        pattern: 0,
        track,
        pitched: true,
    });
}

#[test]
fn a_scan_finds_a_plugin_and_says_what_it_is() {
    let found = desk().scan();
    let ours = found
        .iter()
        .find(|one| one.id == TEST_PLUGIN)
        .unwrap_or_else(|| panic!("the scan did not find the test plugin: {found:?}"));
    assert_eq!(ours.name, "Weetbeats Test Tone");
    assert_eq!(ours.vendor, "Weetbeats");
    assert!(ours.instrument, "an instrument did not say it was one");
    assert!(
        ours.path.ends_with(".clap"),
        "the path is not the file it came from: {}",
        ours.path
    );
}

#[test]
fn a_plugin_track_makes_a_sound_when_a_note_reaches_it() {
    let mut rig = Rig::new(120.0, 16);
    plugin_track(&mut rig, 2);
    rig.send(Command::SetNote {
        pattern: 0,
        track: 2,
        note: note(0, 60, 2),
    });

    // Nothing before the transport starts.
    assert!(
        peak(&rig.render(2048)) < 1e-6,
        "the plugin made a noise while stopped"
    );

    rig.send(Command::SetPlaying(true));
    let out = rig.render_chunked(6000, 256);
    assert!(
        peak(&out) > 0.01,
        "a note reached nothing: the plugin was silent"
    );
}

#[test]
fn a_note_stops_when_it_ends() {
    // 120bpm at 48k is 6000 frames a step. A note two steps long is over after 12000.
    let mut rig = Rig::new(120.0, 16);
    plugin_track(&mut rig, 3);
    rig.send(Command::SetNote {
        pattern: 0,
        track: 3,
        note: note(0, 60, 2),
    });
    rig.send(Command::SetPlaying(true));

    let out = rig.render_chunked(30_000, 256);
    assert!(
        peak_between(&out, 1000, 11_000) > 0.01,
        "the note never sounded"
    );
    // A synth holds a note until it is told otherwise, so this is the note off arriving.
    assert!(
        peak_between(&out, 14_000, 30_000) < 1e-5,
        "the note hung on past its end: {}",
        peak_between(&out, 14_000, 30_000)
    );
}

#[test]
fn stopping_lets_go_of_everything_the_plugin_is_holding() {
    let mut rig = Rig::new(120.0, 16);
    plugin_track(&mut rig, 4);
    // Long enough that it is still ringing when the transport stops.
    rig.send(Command::SetNote {
        pattern: 0,
        track: 4,
        note: note(0, 60, 16),
    });
    rig.send(Command::SetPlaying(true));
    assert!(
        peak(&rig.render_chunked(6000, 256)) > 0.01,
        "the note never sounded"
    );

    rig.send(Command::SetPlaying(false));
    // The first block still has the tail of the note off in it; after that, nothing.
    rig.render(1024);
    assert!(
        peak(&rig.render_chunked(8000, 256)) < 1e-5,
        "the plugin was still holding a note after the transport stopped"
    );
}

#[test]
fn a_muted_pattern_sends_a_plugin_no_notes_at_all() {
    let mut rig = Rig::new(120.0, 16);
    plugin_track(&mut rig, 5);
    rig.send(Command::SetNote {
        pattern: 0,
        track: 5,
        note: note(0, 60, 4),
    });
    rig.send(Command::SetPatternMuted {
        pattern: 0,
        track: 5,
        muted: true,
    });
    rig.send(Command::SetPlaying(true));
    assert!(
        peak(&rig.render_chunked(12_000, 256)) < 1e-5,
        "a muted pattern still reached the plugin"
    );
}

#[test]
fn muting_part_way_through_lets_go_of_the_note() {
    let mut rig = Rig::new(120.0, 16);
    plugin_track(&mut rig, 6);
    rig.send(Command::SetNote {
        pattern: 0,
        track: 6,
        note: note(0, 60, 16),
    });
    rig.send(Command::SetPlaying(true));
    assert!(
        peak(&rig.render_chunked(6000, 256)) > 0.01,
        "the note never sounded"
    );

    // A sampler voice is faded out by its fader. A plugin note has to be let go of, or it
    // hangs on right through the mute.
    rig.send(Command::SetPatternMuted {
        pattern: 0,
        track: 6,
        muted: true,
    });
    rig.render(1024);
    assert!(
        peak(&rig.render_chunked(8000, 256)) < 1e-5,
        "a note started before the mute was still ringing after it"
    );
}

#[test]
fn a_pattern_fader_is_applied_to_the_notes_going_in() {
    let loud = {
        let mut rig = Rig::new(120.0, 16);
        plugin_track(&mut rig, 7);
        rig.send(Command::SetPatternGain {
            pattern: 0,
            track: 7,
            gain: 1.0,
        });
        rig.send(Command::SetNote {
            pattern: 0,
            track: 7,
            note: note(0, 60, 4),
        });
        rig.send(Command::SetPlaying(true));
        peak(&rig.render_chunked(6000, 256))
    };
    let quiet = {
        let mut rig = Rig::new(120.0, 16);
        plugin_track(&mut rig, 8);
        rig.send(Command::SetPatternGain {
            pattern: 0,
            track: 8,
            gain: 0.25,
        });
        rig.send(Command::SetNote {
            pattern: 0,
            track: 8,
            note: note(0, 60, 4),
        });
        rig.send(Command::SetPlaying(true));
        peak(&rig.render_chunked(6000, 256))
    };
    assert!(loud > 0.01, "the loud one was silent");
    assert!(
        quiet < loud * 0.5,
        "turning the pattern down did not quieten the plugin: {quiet} vs {loud}"
    );
}

#[test]
fn a_plugins_parameters_can_be_read_and_moved() {
    let mut rig = Rig::new(120.0, 16);
    plugin_track(&mut rig, 9);

    let params = desk().params(9);
    let level = params
        .iter()
        .find(|p| p.name == "Level")
        .unwrap_or_else(|| panic!("the plugin's parameters did not come back: {params:?}"));
    assert_eq!(level.min, 0.0);
    assert_eq!(level.max, 1.0);
    assert!(
        (level.value - 0.5).abs() < 1e-6,
        "the level did not start where the plugin says it does: {}",
        level.value
    );
    // The plugin says what its value means, which is what the editor shows.
    assert_eq!(level.text, "50%");

    rig.send(Command::SetNote {
        pattern: 0,
        track: 9,
        note: note(0, 60, 8),
    });
    rig.send(Command::SetPlaying(true));
    let before = peak(&rig.render_chunked(6000, 256));

    rig.send(Command::SetPluginParam {
        track: 9,
        param: level.id,
        value: 1.0,
    });
    let after = peak(&rig.render_chunked(6000, 256));
    assert!(
        after > before * 1.5,
        "turning the level up did not reach the plugin: {before} then {after}"
    );

    // And the plugin thread sees where it ended up, which is how the editor picks up a value
    // the plugin changed by itself.
    let moved = desk().params(9);
    let level = moved.iter().find(|p| p.name == "Level").unwrap();
    assert!(
        (level.value - 1.0).abs() < 1e-6,
        "the plugin thread still thinks the level is {}",
        level.value
    );
}

#[test]
fn a_plugins_own_settings_survive_being_saved_and_loaded() {
    let mut rig = Rig::new(120.0, 16);
    plugin_track(&mut rig, 10);

    let level = desk()
        .params(10)
        .into_iter()
        .find(|p| p.name == "Level")
        .unwrap();
    rig.send(Command::SetPluginParam {
        track: 10,
        param: level.id,
        value: 0.125,
    });
    // The parameter arrives with the next block, so give it one.
    rig.render(512);

    let saved = desk().save_state(10).expect("the plugin saved nothing");
    assert!(!saved.is_empty(), "the plugin saved an empty blob");

    // A fresh instance, on another track, given those settings on the way in.
    let loaded = desk()
        .load(11, &fixture().file, TEST_PLUGIN, Some(saved))
        .expect("the test plugin would not load with its settings");
    let level = loaded
        .params
        .iter()
        .find(|p| p.name == "Level")
        .expect("no level parameter after loading");
    assert!(
        (level.value - 0.125).abs() < 1e-6,
        "the saved level did not come back: {}",
        level.value
    );
}

#[test]
fn a_plugin_that_is_not_there_is_a_message_rather_than_a_crash() {
    let missing = desk().load(12, &fixture().file, "com.nobody.nothing", None);
    assert!(
        missing.is_err(),
        "loading a plugin that is not there worked"
    );

    let elsewhere = desk().load(
        12,
        std::path::Path::new("/nowhere/at/all.clap"),
        TEST_PLUGIN,
        None,
    );
    assert!(
        elsewhere.is_err(),
        "loading from a file that is not there worked"
    );
}

// --- the plugin's own window ------------------------------------------------

/// A studio of its own, on this test's thread.
///
/// The window tests do not share the desk the others use, because what they are about is one
/// studio's idea of what is open. A studio has to stay on one thread and this is one, which is
/// all [`Studio`] asks — the *app* additionally needs it on the process's first thread, so a
/// window can be made, and that part is the app's to arrange and not something a test can
/// stand in for.
fn own_studio() -> Studio {
    // Touch the shared desk first, so the fixture is copied and CLAP_PATH is set exactly once.
    let _ = desk();
    Studio::new(RATE as f64)
}

#[test]
fn a_plugin_with_no_window_says_so_rather_than_pretending() {
    let mut studio = own_studio();
    studio
        .load(0, &fixture().file, TEST_PLUGIN, None)
        .expect("the test plugin would not load");

    // The test plugin is a synth with no interface at all, which is exactly the case the app
    // has to have an answer for: the only way to find out is to ask.
    // SAFETY: no parent window is given, so there is nothing that has to outlive anything.
    let asked = unsafe { studio.open_window(0, None, "Test Tone") };
    assert!(
        asked.is_err(),
        "a plugin with no window claimed to have opened one"
    );
    assert!(
        asked.unwrap_err().contains("no window"),
        "the reason did not say what was wrong"
    );
    assert!(!studio.window_open(0), "it thinks a window is up");

    // And shutting one that was never open is quietly nothing, rather than a panic.
    studio.close_window(0);
    assert!(!studio.window_open(0));
}

#[test]
fn a_window_on_a_track_with_no_plugin_is_a_message() {
    let mut studio = own_studio();
    // SAFETY: no parent window is given.
    let asked = unsafe { studio.open_window(3, None, "Nothing") };
    assert!(
        asked.is_err(),
        "opened a window for a plugin that is not there"
    );
    assert!(!studio.window_open(3));
}

#[test]
fn the_host_calls_the_timers_a_plugin_registers() {
    // How a plugin's window repaints, and the one part of window hosting that can be checked
    // without a window: the test plugin asks for a timer as it loads and counts the calls,
    // and reports the count as a parameter so it can be read from out here.
    let mut studio = own_studio();
    studio
        .load(0, &fixture().file, TEST_PLUGIN, None)
        .expect("the test plugin would not load");

    let ticks = |studio: &mut Studio| {
        studio
            .params(0)
            .into_iter()
            .find(|p| p.name == "Ticks")
            .map(|p| p.value as u32)
            .expect("the test plugin lost its tick count")
    };

    assert_eq!(ticks(&mut studio), 0, "the timer fired before it was due");

    // A round of the main thread, several times over, with long enough between them for the
    // timer to come due.
    for _ in 0..5 {
        std::thread::sleep(std::time::Duration::from_millis(20));
        studio.tick();
    }
    let counted = ticks(&mut studio);
    assert!(
        counted >= 4,
        "the host called the plugin's timer {counted} times out of five rounds"
    );

    // And a timer is not called faster than the main thread comes round, however short a
    // period the plugin asked for: the plugin wants one every five milliseconds and gets one
    // per round, because that is all there is.
    assert!(
        counted <= 6,
        "the timer fired {counted} times in five rounds, which is more rounds than there were"
    );

    // Unloading takes the plugin's timers with it, so nothing is left being called.
    studio.unload(0);
    studio.tick();
}
