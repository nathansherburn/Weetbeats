//! The voice pool. A voice is a sample and a read position, plus just enough envelope to
//! stop it clicking.
//!
//! Fixed size, allocated once. When every voice is busy the oldest gets stolen — but not
//! instantly, because cutting a sounding voice dead is exactly what a click is. The stolen
//! voice fades out over [`STEAL_FADE_FRAMES`] and the new note starts when it lands.

use std::sync::Arc;

use crate::command::TrashBin;
use crate::sample::Sample;
use crate::{MAX_VOICES, PREVIEW_TRACK};

/// Fade in at the start of every note. Two hundred frames is about 4ms at 48k, enough to
/// swallow the step in a sample that does not start at zero.
const ATTACK_FRAMES: f32 = 96.0;

/// Fade out when a voice is stolen or the transport stops.
pub const STEAL_FADE_FRAMES: f32 = 128.0;

/// What a voice is doing right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    /// Free.
    Idle,
    /// Fading in.
    Attack,
    /// At full level, playing out.
    Playing,
    /// Fading out. When it reaches zero it either goes idle or starts `pending`.
    Releasing,
}

/// A note asking for a voice.
#[derive(Clone)]
pub struct Trigger {
    pub sample: Arc<Sample>,
    pub track: u16,
    /// Frames of source audio per frame of output: device rate and pitch rolled together.
    pub ratio: f64,
    pub gain: f32,
}

/// One playing sample.
struct Voice {
    sample: Option<Arc<Sample>>,
    /// Read position in source frames. Fractional, because pitch.
    pos: f64,
    ratio: f64,
    gain: f32,
    track: u16,
    /// Bigger is newer. Used to pick who gets stolen.
    age: u64,
    /// Current envelope level, 0.0 to 1.0.
    env: f32,
    stage: Stage,
    /// A note waiting for this voice to finish fading out.
    pending: Option<Trigger>,
    pending_age: u64,
}

impl Voice {
    const fn idle() -> Self {
        Voice {
            sample: None,
            pos: 0.0,
            ratio: 1.0,
            gain: 1.0,
            track: PREVIEW_TRACK,
            age: 0,
            env: 0.0,
            stage: Stage::Idle,
            pending: None,
            pending_age: 0,
        }
    }

    #[inline]
    fn is_free(&self) -> bool {
        self.stage == Stage::Idle
    }

    fn start(&mut self, trigger: Trigger, age: u64) {
        self.sample = Some(trigger.sample);
        self.pos = 0.0;
        self.ratio = trigger.ratio;
        self.gain = trigger.gain;
        self.track = trigger.track;
        self.age = age;
        self.env = 0.0;
        self.stage = Stage::Attack;
    }

    /// Start fading out. Anything already fading keeps its level so it cannot jump.
    fn release(&mut self) {
        if self.stage != Stage::Idle {
            self.stage = Stage::Releasing;
        }
    }

    /// Give the voice up, handing the sample back to the app thread rather than dropping it.
    fn stop(&mut self, trash: &mut TrashBin) {
        if let Some(sample) = self.sample.take() {
            trash.put(sample);
        }
        self.stage = Stage::Idle;
        self.env = 0.0;
        self.pos = 0.0;
    }
}

/// Every voice in the app, and the mixing loop that runs them.
pub struct VoicePool {
    voices: [Voice; MAX_VOICES],
    /// Monotonic counter that decides who is oldest.
    next_age: u64,
    attack_inc: f32,
    release_inc: f32,
}

impl VoicePool {
    pub fn new() -> Self {
        VoicePool {
            voices: [const { Voice::idle() }; MAX_VOICES],
            next_age: 1,
            attack_inc: 1.0 / ATTACK_FRAMES,
            release_inc: 1.0 / STEAL_FADE_FRAMES,
        }
    }

    /// Voices making sound. Includes ones fading out.
    pub fn active(&self) -> u32 {
        self.voices.iter().filter(|v| !v.is_free()).count() as u32
    }

    /// Give a note a voice. Takes a free one if there is one, otherwise steals the oldest.
    pub fn trigger(&mut self, trigger: Trigger) {
        let age = self.next_age;
        self.next_age += 1;

        if let Some(voice) = self.voices.iter_mut().find(|v| v.is_free()) {
            voice.start(trigger, age);
            return;
        }

        // Pool is full. Steal the oldest voice that is not already on its way out, so two
        // stolen notes in a row do not fight over the same slot.
        let mut victim = None;
        let mut oldest = u64::MAX;
        for (i, voice) in self.voices.iter().enumerate() {
            if voice.stage != Stage::Releasing && voice.age < oldest {
                oldest = voice.age;
                victim = Some(i);
            }
        }
        if victim.is_none() {
            // Everything is already fading out. Queue behind whichever has waited longest.
            let mut oldest = u64::MAX;
            for (i, voice) in self.voices.iter().enumerate() {
                if voice.pending_age < oldest {
                    oldest = voice.pending_age;
                    victim = Some(i);
                }
            }
        }

        if let Some(i) = victim {
            let voice = &mut self.voices[i];
            voice.release();
            voice.pending = Some(trigger);
            voice.pending_age = age;
        }
    }

    /// Fade out every voice on a track. Used when a track is deleted or its sample changes.
    pub fn release_track(&mut self, track: u16) {
        for voice in self.voices.iter_mut() {
            if voice.track == track {
                voice.pending = None;
                voice.release();
            }
        }
    }

    /// Fade out everything.
    pub fn release_all(&mut self) {
        for voice in self.voices.iter_mut() {
            voice.pending = None;
            voice.release();
        }
    }

    /// Mix `frames` frames of every voice into an interleaved stereo buffer.
    ///
    /// Track gain arrives as a value at the start of the block plus a per-frame increment,
    /// so a moving fader slides across the block instead of stepping at its edge. Hot loop:
    /// no allocation, no branching that could be hoisted, nothing that can block.
    pub fn render(
        &mut self,
        out: &mut [f32],
        frames: usize,
        track_gain: &[f32],
        track_gain_inc: &[f32],
        trash: &mut TrashBin,
    ) {
        for voice in self.voices.iter_mut() {
            if voice.is_free() {
                continue;
            }
            let Some(sample) = voice.sample.as_ref() else {
                voice.stage = Stage::Idle;
                continue;
            };

            // Track gain of a preview voice is unity: it belongs to no track.
            let slot = voice.track as usize;
            let gain = voice.gain * track_gain.get(slot).copied().unwrap_or(1.0);
            let gain_inc = voice.gain * track_gain_inc.get(slot).copied().unwrap_or(0.0);
            let end = sample.frames as f64;

            for frame in 0..frames {
                match voice.stage {
                    Stage::Attack => {
                        voice.env += self.attack_inc;
                        if voice.env >= 1.0 {
                            voice.env = 1.0;
                            voice.stage = Stage::Playing;
                        }
                    }
                    Stage::Releasing => {
                        voice.env -= self.release_inc;
                        if voice.env <= 0.0 {
                            voice.env = 0.0;
                            break;
                        }
                    }
                    _ => {}
                }

                let (l, r) = sample.frame(voice.pos);
                let level = voice.env * (gain + gain_inc * frame as f32);
                out[frame * 2] += l * level;
                out[frame * 2 + 1] += r * level;

                voice.pos += voice.ratio;
                if voice.pos >= end {
                    voice.stage = Stage::Releasing;
                    voice.env = 0.0;
                    break;
                }
            }

            // Reaching zero either frees the voice or lets the note that stole it start.
            if voice.stage == Stage::Releasing && voice.env <= 0.0 {
                match voice.pending.take() {
                    Some(next) => {
                        let age = voice.pending_age;
                        if let Some(old) = voice.sample.take() {
                            trash.put(old);
                        }
                        voice.start(next, age);
                    }
                    None => voice.stop(trash),
                }
            }
        }
    }

    /// Hand every sample back and go quiet. For shutdown.
    pub fn clear(&mut self, trash: &mut TrashBin) {
        for voice in self.voices.iter_mut() {
            voice.pending = None;
            voice.stop(trash);
        }
    }
}

impl Default for VoicePool {
    fn default() -> Self {
        Self::new()
    }
}
