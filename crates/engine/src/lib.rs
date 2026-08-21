//! The Weetbeats audio engine.
//!
//! Split in two halves that never share a lock:
//!
//! * [`model`] is the project — plain data, serde-friendly, owned by the app thread.
//! * [`engine::Engine`] is the audio thread. It owns a fixed pool of voices, a step clock
//!   driven by the sample count, and a mixer. It is fed [`command::Command`]s through a
//!   lock free ring buffer and reports back through [`shared::Shared`] atomics.
//!
//! ## The one rule
//!
//! [`Engine::render`](engine::Engine::render) must never allocate, lock, block or touch a
//! file. Everything it needs is prepared elsewhere and handed over. `tests/no_alloc.rs`
//! holds it to that with a counting global allocator, so if you break the rule a test
//! fails instead of your ears.

pub mod clock;
pub mod command;
pub mod engine;
pub mod folder;
pub mod model;
pub mod sample;
pub mod shared;
pub mod voice;

/// Track slots the engine keeps state for. A track's id in the project *is* its slot.
pub const MAX_TRACKS: usize = 32;

/// Notes the engine holds per track. The UI refuses to add more.
pub const MAX_NOTES_PER_TRACK: usize = 256;

/// Voices in the pool. Run out and the oldest gets stolen.
pub const MAX_VOICES: usize = 64;

/// Longest pattern the engine will play.
pub const MAX_STEPS: u16 = 64;

/// Boxes a new pattern has.
pub const DEFAULT_STEPS: u32 = 16;

/// Pattern slots the engine keeps notes for. A pattern's id in the project *is* its slot,
/// the same trick as tracks, so the engine never has to be told a pattern has moved.
pub const MAX_PATTERNS: usize = 32;

/// Slots in the song. One slot is one whole pattern, so this is long enough for a song of
/// two hundred and fifty six patterns end to end.
pub const MAX_SONG_SLOTS: usize = 256;

/// Frames the mixer works on at a time. Longer callbacks get chopped into these.
pub const MAX_BLOCK: usize = 1024;

/// Steps in one beat. Sixteenth notes, i.e. 16 steps to a 4/4 bar.
pub const STEPS_PER_BEAT: f64 = 4.0;

/// The pitch a step box means. Middle C, and the sampler's unity pitch.
pub const DEFAULT_PITCH: u8 = 60;

/// Voice slot marker for auditioned samples that belong to no track.
pub const PREVIEW_TRACK: u16 = u16::MAX;

pub use command::{Command, EngineNote, Trash};
pub use engine::Engine;
pub use model::{Lane, Note, Pattern, Project, SampleRef, Track};
pub use sample::Sample;
pub use shared::{Playhead, Shared};

/// Semitones to a playback rate multiplier, relative to [`DEFAULT_PITCH`].
#[inline]
pub fn pitch_ratio(pitch: u8) -> f64 {
    // 2^(semitones/12). exp2 is a single instruction on both targets we care about.
    (((pitch as f64) - (DEFAULT_PITCH as f64)) / 12.0).exp2()
}

/// MIDI velocity to a linear gain. Squared, because that tracks loudness better than linear.
#[inline]
pub fn velocity_gain(velocity: u8) -> f32 {
    let v = (velocity as f32) / 127.0;
    v * v
}

/// Cubic soft clip. Unity for quiet signals, hard ceiling at 1.0, no discontinuity between.
#[inline]
pub fn soft_clip(x: f32) -> f32 {
    if x <= -1.0 {
        -1.0
    } else if x >= 1.0 {
        1.0
    } else {
        1.5 * (x - (x * x * x) / 3.0)
    }
}
