//! The voice pool. A voice is a sample, a read position, and the envelope its track's
//! [`Voicing`](crate::model::Voicing) gives it.
//!
//! Fixed size, allocated once. When every voice is busy the oldest gets stolen — but not
//! instantly, because cutting a sounding voice dead is exactly what a click is. The stolen
//! voice fades out over [`STEAL_FADE_FRAMES`] and the new note starts when it lands.
//!
//! ## The envelope
//!
//! Attack, decay, sustain, release, with the times arriving as per-frame increments so the
//! inner loop is an add and a compare. A sound nobody has shaped has a two millisecond
//! attack, no decay, full sustain and a three millisecond release, which is the fade at each
//! end that used to be hard coded: the default sounds exactly like it did before there was
//! an envelope to change.
//!
//! Release comes from two places and they are not the same thing. A note that has ended
//! releases over its own release time, however long the sound asks for. A voice that has been
//! *stolen* releases over [`STEAL_FADE_FRAMES`] whatever the sound says, because something
//! else is waiting for the slot and a two second pad release would hold it for two seconds.

use std::sync::Arc;

use crate::command::TrashBin;
use crate::sample::Sample;
use crate::{MAX_TRACKS, MAX_VOICES, PREVIEW_PATTERN, PREVIEW_TRACK};

/// Fade out when a voice is stolen or the transport stops. Short on purpose: whoever stole
/// the voice is waiting for it.
pub const STEAL_FADE_FRAMES: f32 = 128.0;

/// The shortest a stage of the envelope can take, in frames. A zero length attack is a step
/// from silence to full, which is a click; this is a quarter of a millisecond at 48k, short
/// enough to be a hard start and long enough not to be heard as one.
const MIN_STAGE_FRAMES: f32 = 12.0;

/// One track's envelope, as the audio thread wants it: per-frame increments rather than
/// seconds, worked out once when the voicing changes rather than on every note.
#[derive(Clone, Copy, Debug)]
pub struct Envelope {
    pub attack_inc: f32,
    pub decay_inc: f32,
    pub sustain: f32,
    pub release_inc: f32,
}

impl Envelope {
    /// Turn seconds into per-frame steps. Called on the app thread, or on the audio thread
    /// when a command arrives — never in the mixing loop.
    pub fn new(voicing: &crate::model::Voicing, sample_rate: f64) -> Self {
        let per_frame = |secs: f32| {
            let frames = (secs as f64 * sample_rate) as f32;
            1.0 / frames.max(MIN_STAGE_FRAMES)
        };
        Envelope {
            attack_inc: per_frame(voicing.attack),
            // Decay falls from full to the sustain level, so a zero decay is a jump to it.
            decay_inc: per_frame(voicing.decay),
            sustain: voicing.sustain.clamp(0.0, 1.0),
            release_inc: per_frame(voicing.release),
        }
    }
}

impl Default for Envelope {
    fn default() -> Self {
        Envelope::new(&crate::model::Voicing::default(), 48_000.0)
    }
}

/// What a voice is doing right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    /// Free.
    Idle,
    /// Rising to full.
    Attack,
    /// Falling from full to the sustain level.
    Decay,
    /// Holding at the sustain level, playing out.
    Playing,
    /// Fading out. When it reaches zero it either goes idle or starts `pending`.
    Releasing,
}

/// A note asking for a voice.
#[derive(Clone)]
pub struct Trigger {
    pub sample: Arc<Sample>,
    pub track: u16,
    /// Which pattern started it, because the mixer belongs to the pattern: a voice has to
    /// know whose fader it is under. [`PREVIEW_PATTERN`] for anything played by hand.
    pub pattern: u16,
    /// Frames of source audio per frame of output: device rate, pitch and the sound's own
    /// tuning rolled together.
    pub ratio: f64,
    pub gain: f32,
    /// How long the note is held, in output frames. `f64::INFINITY` for a one-shot, which
    /// is a drum: the sample rings out and the note's length means nothing.
    pub frames: f64,
    /// The shape the track's voicing gives it.
    pub envelope: Envelope,
    /// Where the sound sits between the speakers, as a gain each side. Worked out once when
    /// the voicing changes, because it costs a sine and a cosine.
    pub left: f32,
    pub right: f32,
    /// The first and last source frame a note reads, for a sound that has been trimmed.
    pub from: f64,
    pub to: f64,
}

/// One playing sample.
struct Voice {
    sample: Option<Arc<Sample>>,
    /// Read position in source frames. Fractional, because pitch.
    pos: f64,
    ratio: f64,
    gain: f32,
    /// Gain each side, from where the sound is panned.
    left: f32,
    right: f32,
    /// The source frame the note stops at, which is the end of the file unless the sound
    /// has been trimmed.
    to: f64,
    track: u16,
    pattern: u16,
    /// Bigger is newer. Used to pick who gets stolen.
    age: u64,
    /// Frames of the note still to go. Counting to zero is the note off.
    frames_left: f64,
    /// Current envelope level, 0.0 to 1.0.
    env: f32,
    /// The shape this note was given, and how fast it is coming down right now — which is
    /// the sound's own release for a note that has ended and the steal fade for one that has
    /// been taken off it.
    envelope: Envelope,
    release_inc: f32,
    stage: Stage,
    /// A note waiting for this voice to finish fading out.
    pending: Option<Trigger>,
    pending_age: u64,
}

impl Voice {
    fn idle() -> Self {
        Voice {
            sample: None,
            pos: 0.0,
            ratio: 1.0,
            gain: 1.0,
            left: 1.0,
            right: 1.0,
            to: f64::INFINITY,
            track: PREVIEW_TRACK,
            pattern: PREVIEW_PATTERN,
            age: 0,
            frames_left: f64::INFINITY,
            env: 0.0,
            envelope: Envelope::default(),
            release_inc: 1.0 / STEAL_FADE_FRAMES,
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
        let end = trigger.sample.frames as f64;
        self.sample = Some(trigger.sample);
        self.pos = trigger.from.clamp(0.0, end);
        self.to = trigger.to.min(end);
        self.ratio = trigger.ratio;
        self.gain = trigger.gain;
        self.left = trigger.left;
        self.right = trigger.right;
        self.track = trigger.track;
        self.pattern = trigger.pattern;
        self.age = age;
        self.frames_left = trigger.frames;
        self.env = 0.0;
        self.envelope = trigger.envelope;
        self.release_inc = trigger.envelope.release_inc;
        self.stage = Stage::Attack;
    }

    /// Start fading out fast, because the slot is wanted or everything is stopping. A long
    /// release is the sound's business; whoever is waiting for the voice is not going to
    /// wait two seconds for it.
    fn release(&mut self) {
        if self.stage != Stage::Idle {
            self.release_inc = (1.0 / STEAL_FADE_FRAMES).max(self.release_inc);
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
}

impl VoicePool {
    pub fn new() -> Self {
        VoicePool {
            voices: std::array::from_fn(|_| Voice::idle()),
            next_age: 1,
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
    /// Gain arrives as a value at the start of the block plus a per-frame increment, one
    /// pair per pattern and track, so a moving fader slides across the block instead of
    /// stepping at its edge — and a mute takes what is already ringing down with it. Hot
    /// loop: no allocation, no branching that could be hoisted, nothing that can block.
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

            // The fader this voice is under: its own pattern's, for its own track. A voice
            // played by hand belongs to no pattern, so it falls off the end of the table and
            // plays at unity, which is what auditioning a sound should do.
            let slot = voice.pattern as usize * MAX_TRACKS + voice.track as usize;
            let gain = voice.gain * track_gain.get(slot).copied().unwrap_or(1.0);
            let gain_inc = voice.gain * track_gain_inc.get(slot).copied().unwrap_or(0.0);
            // Where this note stops reading: the end of the file, or wherever the sound has
            // been trimmed to.
            let end = voice.to;

            for frame in 0..frames {
                // The note off, over the sound's own release rather than the steal fade: a
                // pad that has been given a long tail keeps it. A one-shot never gets here:
                // its length is infinite, because a drum hit is over when the sample is over
                // and not before.
                //
                // Written out field by field rather than as a method because `sample` above
                // is a borrow of one of this voice's fields, and a method would want the lot.
                if voice.frames_left <= 0.0 && voice.stage != Stage::Releasing {
                    voice.release_inc = voice.envelope.release_inc;
                    voice.stage = Stage::Releasing;
                }
                voice.frames_left -= 1.0;

                match voice.stage {
                    Stage::Attack => {
                        voice.env += voice.envelope.attack_inc;
                        if voice.env >= 1.0 {
                            voice.env = 1.0;
                            voice.stage = Stage::Decay;
                        }
                    }
                    Stage::Decay => {
                        voice.env -= voice.envelope.decay_inc;
                        if voice.env <= voice.envelope.sustain {
                            voice.env = voice.envelope.sustain;
                            voice.stage = Stage::Playing;
                        }
                    }
                    Stage::Releasing => {
                        voice.env -= voice.release_inc;
                        if voice.env <= 0.0 {
                            voice.env = 0.0;
                            break;
                        }
                    }
                    _ => {}
                }

                let (l, r) = sample.frame(voice.pos);
                let level = voice.env * (gain + gain_inc * frame as f32);
                out[frame * 2] += l * level * voice.left;
                out[frame * 2 + 1] += r * level * voice.right;

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
