//! The audio thread.
//!
//! [`Engine::render`] is called by the audio device and nothing else. It must never
//! allocate, lock, block, or touch a file — a stall of even a few milliseconds is a click.
//! Everything it needs arrives ready-made through the command queue.

use std::sync::Arc;

use rtrb::Consumer;

use crate::clock::StepClock;
use crate::command::{Command, EngineNote, TrashBin};
use crate::sample::Sample;
use crate::shared::Shared;
use crate::voice::{Trigger, VoicePool};
use crate::{
    pitch_ratio, soft_clip, velocity_gain, MAX_BLOCK, MAX_NOTES_PER_TRACK, MAX_TRACKS,
    PREVIEW_TRACK,
};

/// How fast a gain change slides to its new value. About 10ms at 48k, which is slow enough
/// to have no zipper noise and fast enough that a slider feels connected.
const GAIN_SMOOTHING_FRAMES: f32 = 480.0;

/// One track's worth of audio thread state. Fixed size: no `Vec`, nothing to grow.
struct TrackState {
    /// False when the slot is free. Free slots are skipped entirely.
    active: bool,
    sample: Option<Arc<Sample>>,
    /// Where the gain slider is.
    target_gain: f32,
    /// Where the gain actually is, chasing the target.
    gain: f32,
    muted: bool,
    soloed: bool,
    notes: [EngineNote; MAX_NOTES_PER_TRACK],
    note_count: usize,
}

impl TrackState {
    const fn empty() -> Self {
        TrackState {
            active: false,
            sample: None,
            target_gain: 0.8,
            gain: 0.8,
            muted: false,
            soloed: false,
            notes: [EngineNote {
                step: 0,
                pitch: 0,
                velocity: 0,
                length: 0,
            }; MAX_NOTES_PER_TRACK],
            note_count: 0,
        }
    }

    fn find(&self, step: u16, pitch: u8) -> Option<usize> {
        self.notes[..self.note_count]
            .iter()
            .position(|n| n.step == step && n.pitch == pitch)
    }

    fn set_note(&mut self, note: EngineNote) {
        match self.find(note.step, note.pitch) {
            Some(i) => self.notes[i] = note,
            None => {
                if self.note_count < MAX_NOTES_PER_TRACK {
                    self.notes[self.note_count] = note;
                    self.note_count += 1;
                }
            }
        }
    }

    fn clear_note(&mut self, step: u16, pitch: u8) {
        if let Some(i) = self.find(step, pitch) {
            // Order does not matter, so fill the hole with the last note.
            self.notes[i] = self.notes[self.note_count - 1];
            self.note_count -= 1;
        }
    }
}

/// The mixer, the clock and the voices. Lives on the audio thread and is only ever touched
/// from there once it has been handed over.
pub struct Engine {
    tracks: [TrackState; MAX_TRACKS],
    voices: VoicePool,
    clock: StepClock,
    playing: bool,
    master_gain: f32,
    master_gain_target: f32,
    sample_rate: f64,
    /// Stereo scratch the voices mix into, before the master stage. Allocated once, here.
    mix: [f32; MAX_BLOCK * 2],
    /// Track gain at the start of the current block, so the voice loop does not walk
    /// `tracks`, and how much it moves per frame across the block.
    gains: [f32; MAX_TRACKS],
    gain_incs: [f32; MAX_TRACKS],
    rx: Consumer<Command>,
    trash: TrashBin,
    shared: Arc<Shared>,
    gain_inc: f32,
}

impl Engine {
    /// Build the engine. Do this on the app thread, then move the box to the audio thread:
    /// it is a fair few kilobytes and this is the only place it gets allocated.
    pub fn new(
        sample_rate: u32,
        bpm: f32,
        steps: u32,
        shared: Arc<Shared>,
        rx: Consumer<Command>,
        trash: TrashBin,
    ) -> Box<Self> {
        Box::new(Engine {
            tracks: [const { TrackState::empty() }; MAX_TRACKS],
            voices: VoicePool::new(),
            clock: StepClock::new(sample_rate, bpm, steps),
            playing: false,
            master_gain: 0.9,
            master_gain_target: 0.9,
            sample_rate: sample_rate.max(1) as f64,
            mix: [0.0; MAX_BLOCK * 2],
            gains: [0.0; MAX_TRACKS],
            gain_incs: [0.0; MAX_TRACKS],
            rx,
            trash,
            shared,
            gain_inc: 1.0 / GAIN_SMOOTHING_FRAMES,
        })
    }

    /// Fill `out` with interleaved audio, `channels` wide.
    ///
    /// The buffer is chopped at step boundaries so notes land on the exact frame they are
    /// due, not at the start of whichever callback happens to contain them.
    pub fn render(&mut self, out: &mut [f32], channels: usize) {
        self.drain_commands();

        let channels = channels.max(1);
        let total_frames = out.len() / channels;
        let mut done = 0usize;
        let mut peak = 0.0f32;

        while done < total_frames {
            if self.playing && self.clock.due() {
                let step = self.clock.take_step();
                self.trigger_step(step);
            }

            let mut frames = (total_frames - done).min(MAX_BLOCK);
            if self.playing {
                frames = frames.min(self.clock.frames_to_next_step());
            }

            let block = &mut out[done * channels..(done + frames) * channels];
            self.render_block(block, channels, frames, &mut peak);

            if self.playing {
                self.clock.advance(frames);
            }
            done += frames;
        }

        // A device that hands over a partial frame gets silence in the offcut rather
        // than whatever was in the buffer before.
        for slot in out[total_frames * channels..].iter_mut() {
            *slot = 0.0;
        }

        self.shared.set_playing(self.playing);
        self.shared
            .set_position(self.clock.step(), self.clock.progress());
        self.shared.set_meters(self.voices.active(), peak);
        self.shared.add_frames(total_frames as u64);
    }

    /// Mix one run of frames that contains no step boundary.
    fn render_block(&mut self, out: &mut [f32], channels: usize, frames: usize, peak: &mut f32) {
        let mix = &mut self.mix[..frames * 2];
        mix.fill(0.0);

        // Solo beats mute: if anything is soloed, only soloed tracks are heard.
        //
        // Gain never jumps to its new value, it slides. A block is anywhere from 64 to
        // 1024 frames, so clamping the change per block would still be a step change at
        // the block boundary — which is a zipper, or on a mute, a click. Instead each
        // track gets a start value and a per-frame increment, and the voice loop
        // interpolates. A full-scale change always takes GAIN_SMOOTHING_FRAMES.
        let any_soloed = self.tracks.iter().any(|t| t.active && t.soloed);
        let max_change = self.gain_inc * frames as f32;
        for (i, track) in self.tracks.iter_mut().enumerate() {
            let audible = track.active
                && if any_soloed {
                    track.soloed
                } else {
                    !track.muted
                };
            let target = if audible { track.target_gain } else { 0.0 };
            let start = track.gain;
            let end = start + (target - start).clamp(-max_change, max_change);
            track.gain = end;
            self.gains[i] = start;
            self.gain_incs[i] = (end - start) / frames as f32;
        }

        self.voices
            .render(mix, frames, &self.gains, &self.gain_incs, &mut self.trash);

        let master_start = self.master_gain;
        let master_end =
            master_start + (self.master_gain_target - master_start).clamp(-max_change, max_change);
        self.master_gain = master_end;
        let master_inc = (master_end - master_start) / frames as f32;

        for frame in 0..frames {
            let master = master_start + master_inc * frame as f32;
            let l = soft_clip(mix[frame * 2] * master);
            let r = soft_clip(mix[frame * 2 + 1] * master);
            let mag = l.abs().max(r.abs());
            if mag > *peak {
                *peak = mag;
            }
            let base = frame * channels;
            match channels {
                1 => out[base] = (l + r) * 0.5,
                2 => {
                    out[base] = l;
                    out[base + 1] = r;
                }
                n => {
                    out[base] = l;
                    out[base + 1] = r;
                    // Surround devices get silence in the rest rather than a copy.
                    for c in 2..n {
                        out[base + c] = 0.0;
                    }
                }
            }
        }
    }

    /// Everything due on this step gets a voice.
    fn trigger_step(&mut self, step: u32) {
        let step = step as u16;
        for track_index in 0..MAX_TRACKS {
            let track = &self.tracks[track_index];
            if !track.active {
                continue;
            }
            let Some(sample) = track.sample.as_ref() else {
                continue;
            };
            for i in 0..track.note_count {
                let note = track.notes[i];
                if note.step != step {
                    continue;
                }
                let trigger = Trigger {
                    sample: Arc::clone(sample),
                    track: track_index as u16,
                    ratio: self.playback_ratio(sample, note.pitch),
                    gain: velocity_gain(note.velocity),
                };
                self.voices.trigger(trigger);
            }
        }
    }

    /// Source frames per output frame: the device rate correction and the pitch, together.
    #[inline]
    fn playback_ratio(&self, sample: &Sample, pitch: u8) -> f64 {
        (sample.source_rate as f64 / self.sample_rate) * pitch_ratio(pitch)
    }

    /// Take everything waiting in the command queue. Popping is a memcpy from a ring
    /// buffer: no locks, no allocation, no chance of blocking the app thread either.
    fn drain_commands(&mut self) {
        while let Ok(command) = self.rx.pop() {
            self.apply(command);
        }
    }

    fn apply(&mut self, command: Command) {
        match command {
            Command::SetPlaying(playing) => {
                self.playing = playing;
                if !playing {
                    // Stop leaves ringing voices to finish; it is not a panic button.
                    self.clock.rewind();
                }
            }
            Command::Rewind => self.clock.rewind(),
            Command::SetBpm(bpm) => self.clock.set_bpm(bpm),
            Command::SetSteps(steps) => self.clock.set_steps(steps),
            Command::SetMasterGain(gain) => self.master_gain_target = gain.clamp(0.0, 2.0),
            Command::AddTrack { track, gain } => {
                if let Some(t) = self.tracks.get_mut(track as usize) {
                    t.active = true;
                    t.note_count = 0;
                    t.muted = false;
                    t.soloed = false;
                    t.target_gain = gain.clamp(0.0, 2.0);
                    t.gain = t.target_gain;
                }
            }
            Command::RemoveTrack { track } => {
                self.voices.release_track(track);
                if let Some(t) = self.tracks.get_mut(track as usize) {
                    t.active = false;
                    t.note_count = 0;
                    if let Some(sample) = t.sample.take() {
                        self.trash.put(sample);
                    }
                }
            }
            Command::SetTrackSample { track, sample } => {
                self.voices.release_track(track);
                if let Some(t) = self.tracks.get_mut(track as usize) {
                    if let Some(old) = t.sample.replace_with(sample) {
                        self.trash.put(old);
                    }
                }
            }
            Command::SetTrackGain { track, gain } => {
                if let Some(t) = self.tracks.get_mut(track as usize) {
                    t.target_gain = gain.clamp(0.0, 2.0);
                }
            }
            Command::SetTrackMuted { track, muted } => {
                if let Some(t) = self.tracks.get_mut(track as usize) {
                    t.muted = muted;
                }
            }
            Command::SetTrackSoloed { track, soloed } => {
                if let Some(t) = self.tracks.get_mut(track as usize) {
                    t.soloed = soloed;
                }
            }
            Command::SetNote { track, note } => {
                if let Some(t) = self.tracks.get_mut(track as usize) {
                    t.set_note(note);
                }
            }
            Command::ClearNote { track, step, pitch } => {
                if let Some(t) = self.tracks.get_mut(track as usize) {
                    t.clear_note(step, pitch);
                }
            }
            Command::ClearNotes { track } => {
                if let Some(t) = self.tracks.get_mut(track as usize) {
                    t.note_count = 0;
                }
            }
            Command::Audition {
                track,
                pitch,
                velocity,
            } => {
                let Some(t) = self.tracks.get(track as usize) else {
                    return;
                };
                let Some(sample) = t.sample.as_ref() else {
                    return;
                };
                let trigger = Trigger {
                    sample: Arc::clone(sample),
                    track,
                    ratio: self.playback_ratio(sample, pitch),
                    gain: velocity_gain(velocity),
                };
                self.voices.trigger(trigger);
            }
            Command::Preview { sample, gain } => {
                let ratio = self.playback_ratio(&sample, crate::DEFAULT_PITCH);
                self.voices.trigger(Trigger {
                    sample,
                    track: PREVIEW_TRACK,
                    ratio,
                    gain,
                });
            }
            Command::StopAll => {
                self.playing = false;
                self.clock.rewind();
                self.voices.release_all();
            }
        }
    }
}

/// `Option::replace` but returning the old value only when there was one, so the caller
/// can bin it without a nested match.
trait ReplaceWith<T> {
    fn replace_with(&mut self, value: Option<T>) -> Option<T>;
}

impl<T> ReplaceWith<T> for Option<T> {
    #[inline]
    fn replace_with(&mut self, value: Option<T>) -> Option<T> {
        std::mem::replace(self, value)
    }
}
