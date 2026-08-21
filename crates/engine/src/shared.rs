//! What the audio thread tells everyone else, without ever waiting for them.
//!
//! Written by the audio thread with plain atomic stores, read by the UI whenever it likes.
//! The front end polls this from `requestAnimationFrame`; nothing is pushed at it, because
//! an event per step would judder.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

/// A reading of where the playhead is, taken by the UI.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Playhead {
    pub playing: bool,
    /// Which step is sounding.
    pub step: u32,
    /// Which patterns are sounding, one bit each. More than one bar at a time in a song.
    pub patterns: u32,
    /// Which bar of the song is playing, when the song is what is playing.
    pub bar: u32,
    /// How far through that step, 0.0 to 1.0. Makes the cursor move smoothly.
    pub progress: f32,
    pub active_voices: u32,
    /// Loudest sample since the last render, for a level meter.
    pub peak: f32,
}

/// Shared between the audio thread and everyone else. No locks, ever.
#[derive(Debug, Default)]
pub struct Shared {
    playing: AtomicBool,
    step: AtomicU32,
    patterns: AtomicU32,
    bar: AtomicU32,
    progress: AtomicU32,
    active_voices: AtomicU32,
    peak: AtomicU32,
    frames: AtomicU64,
    /// Counts samples the audio thread had to drop on the floor instead of returning to
    /// the app thread. Should stay at zero; if it climbs, the trash queue is too small.
    dropped_on_audio_thread: AtomicU32,
}

impl Shared {
    pub fn new() -> Self {
        Self::default()
    }

    /// One consistent-enough snapshot for the UI. Fields are read independently, so they
    /// can be a few microseconds apart. For drawing a playhead that is invisible.
    pub fn playhead(&self) -> Playhead {
        Playhead {
            playing: self.playing.load(Ordering::Relaxed),
            step: self.step.load(Ordering::Relaxed),
            patterns: self.patterns.load(Ordering::Relaxed),
            bar: self.bar.load(Ordering::Relaxed),
            progress: f32::from_bits(self.progress.load(Ordering::Relaxed)),
            active_voices: self.active_voices.load(Ordering::Relaxed),
            peak: f32::from_bits(self.peak.load(Ordering::Relaxed)),
        }
    }

    /// Frames rendered since the stream started. The only honest clock in the app.
    pub fn frames_rendered(&self) -> u64 {
        self.frames.load(Ordering::Relaxed)
    }

    pub fn dropped_on_audio_thread(&self) -> u32 {
        self.dropped_on_audio_thread.load(Ordering::Relaxed)
    }

    // --- audio thread side ---

    #[inline]
    pub(crate) fn set_playing(&self, playing: bool) {
        self.playing.store(playing, Ordering::Relaxed);
    }

    #[inline]
    pub(crate) fn set_position(&self, step: u32, progress: f32, patterns: u32, bar: u32) {
        self.step.store(step, Ordering::Relaxed);
        self.progress.store(progress.to_bits(), Ordering::Relaxed);
        self.patterns.store(patterns, Ordering::Relaxed);
        self.bar.store(bar, Ordering::Relaxed);
    }

    #[inline]
    pub(crate) fn set_meters(&self, active_voices: u32, peak: f32) {
        self.active_voices.store(active_voices, Ordering::Relaxed);
        self.peak.store(peak.to_bits(), Ordering::Relaxed);
    }

    #[inline]
    pub(crate) fn add_frames(&self, frames: u64) {
        self.frames.fetch_add(frames, Ordering::Relaxed);
    }

    #[inline]
    pub(crate) fn note_dropped(&self) {
        self.dropped_on_audio_thread.fetch_add(1, Ordering::Relaxed);
    }
}
