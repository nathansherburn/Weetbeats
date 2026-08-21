//! The audio thread.
//!
//! [`Engine::render`] is called by the audio device and nothing else. It must never
//! allocate, lock, block, or touch a file — a stall of even a few milliseconds is a click.
//! Everything it needs arrives ready-made through the command queue.
//!
//! ## Every pattern lives here
//!
//! The engine holds the notes of every pattern, not just the one playing. It has to: at the
//! end of a pattern the song moves on to the next one *on the boundary frame*, and there is
//! no time to ask the app thread for it. So a pattern change is a change of index, which
//! costs nothing.
//!
//! What plays depends on the mode. In pattern mode the open pattern loops, which is the
//! pattern editor. In song mode the engine walks the song, and each slot plays one whole
//! pattern, however long that pattern is.

use std::sync::Arc;

use rtrb::Consumer;

use crate::clock::StepClock;
use crate::command::{Command, EngineNote, TrashBin};
use crate::sample::Sample;
use crate::shared::Shared;
use crate::voice::{Trigger, VoicePool};
use crate::{
    pitch_ratio, soft_clip, velocity_gain, MAX_BLOCK, MAX_NOTES_PER_TRACK, MAX_PATTERNS,
    MAX_SONG_SLOTS, MAX_STEPS, MAX_TRACKS, PREVIEW_TRACK,
};

/// How fast a gain change slides to its new value. About 10ms at 48k, which is slow enough
/// to have no zipper noise and fast enough that a slider feels connected.
const GAIN_SMOOTHING_FRAMES: f32 = 480.0;

/// One track's worth of audio thread state. Fixed size: no `Vec`, nothing to grow.
///
/// Notes are not in here. A track is a sound and how loud it is, which is the project's
/// business; the notes belong to whichever pattern they were drawn in.
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
        }
    }
}

/// One track's notes in one pattern.
struct NoteList {
    notes: [EngineNote; MAX_NOTES_PER_TRACK],
    count: usize,
}

impl NoteList {
    fn empty() -> Self {
        NoteList {
            notes: [EngineNote {
                step: 0,
                pitch: 0,
                velocity: 0,
                length: 0,
            }; MAX_NOTES_PER_TRACK],
            count: 0,
        }
    }

    fn find(&self, step: u16, pitch: u8) -> Option<usize> {
        self.notes[..self.count]
            .iter()
            .position(|n| n.step == step && n.pitch == pitch)
    }

    fn set(&mut self, note: EngineNote) {
        match self.find(note.step, note.pitch) {
            Some(i) => self.notes[i] = note,
            None => {
                if self.count < MAX_NOTES_PER_TRACK {
                    self.notes[self.count] = note;
                    self.count += 1;
                }
            }
        }
    }

    fn clear_one(&mut self, step: u16, pitch: u8) {
        if let Some(i) = self.find(step, pitch) {
            // Order does not matter, so fill the hole with the last note.
            self.notes[i] = self.notes[self.count - 1];
            self.count -= 1;
        }
    }
}

/// One pattern: how long it is, and what every track plays in it.
struct PatternState {
    steps: u32,
    tracks: Vec<NoteList>,
}

impl PatternState {
    /// The `Vec`s here are the point of taking `steps` in [`Engine::new`]: they are
    /// allocated once, on the app thread, and only ever indexed after that.
    fn new(steps: u32) -> Self {
        PatternState {
            steps,
            tracks: (0..MAX_TRACKS).map(|_| NoteList::empty()).collect(),
        }
    }
}

/// The mixer, the clock and the voices. Lives on the audio thread and is only ever touched
/// from there once it has been handed over.
pub struct Engine {
    tracks: [TrackState; MAX_TRACKS],
    /// Every pattern's notes. Indexed by pattern id, never resized after `new`.
    patterns: Vec<PatternState>,
    /// The song: a pattern id per slot. One slot is one whole pattern.
    song: [u16; MAX_SONG_SLOTS],
    song_len: usize,
    song_slot: usize,
    /// True to play the song, false to loop the pattern the editor has open.
    song_mode: bool,
    active_pattern: usize,
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
    /// it is a megabyte or two of notes and this is the only place any of it is allocated.
    pub fn new(
        sample_rate: u32,
        bpm: f32,
        steps: u32,
        shared: Arc<Shared>,
        rx: Consumer<Command>,
        trash: TrashBin,
    ) -> Box<Self> {
        let steps = steps.clamp(1, MAX_STEPS as u32);
        Box::new(Engine {
            tracks: [const { TrackState::empty() }; MAX_TRACKS],
            patterns: (0..MAX_PATTERNS)
                .map(|_| PatternState::new(steps))
                .collect(),
            song: [0; MAX_SONG_SLOTS],
            song_len: 0,
            song_slot: 0,
            song_mode: false,
            active_pattern: 0,
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
    /// due, not at the start of whichever callback happens to contain them. That is also
    /// what lets the song move on to the next pattern on the right frame.
    pub fn render(&mut self, out: &mut [f32], channels: usize) {
        self.drain_commands();

        let channels = channels.max(1);
        let total_frames = out.len() / channels;
        let mut done = 0usize;
        let mut peak = 0.0f32;

        while done < total_frames {
            if self.playing && self.clock.due() {
                let step = self.clock.take_step();
                self.trigger_step(step as u16);
            }

            let mut frames = (total_frames - done).min(MAX_BLOCK);
            if self.playing {
                frames = frames.min(self.clock.frames_to_next_step());
            }

            let block = &mut out[done * channels..(done + frames) * channels];
            self.render_block(block, channels, frames, &mut peak);

            if self.playing {
                self.clock.advance(frames);
                if self.clock.take_wrapped() {
                    self.next_slot();
                }
            }
            done += frames;
        }

        // A device that hands over a partial frame gets silence in the offcut rather
        // than whatever was in the buffer before.
        for slot in out[total_frames * channels..].iter_mut() {
            *slot = 0.0;
        }

        self.shared.set_playing(self.playing);
        self.shared.set_position(
            self.clock.step(),
            self.clock.progress(),
            self.sounding_pattern() as u32,
            self.song_slot as u32,
        );
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

    // --- what is playing ---------------------------------------------------

    /// The pattern whose notes are due to sound, or `None` when the song is empty and
    /// there is nothing to play.
    #[inline]
    fn playing_pattern(&self) -> Option<usize> {
        if self.song_mode {
            if self.song_len == 0 {
                return None;
            }
            let slot = self.song_slot.min(self.song_len - 1);
            Some(self.song[slot] as usize)
        } else {
            Some(self.active_pattern)
        }
    }

    /// What to tell the UI the playhead is in. An empty song still has to answer.
    #[inline]
    fn sounding_pattern(&self) -> usize {
        self.playing_pattern().unwrap_or(self.active_pattern)
    }

    /// Point the clock at whatever is playing now. A shorter pattern pulls the playhead
    /// back to the top rather than leaving it past the end.
    fn tune_clock(&mut self) {
        if let Some(pattern) = self.playing_pattern() {
            let steps = self.patterns[pattern].steps;
            self.clock.set_steps(steps);
        }
    }

    /// Back to the top: the first slot of the song, and the first step of it.
    fn rewind_all(&mut self) {
        self.song_slot = 0;
        self.tune_clock();
        self.clock.rewind();
    }

    /// The pattern just finished, so the song moves on. In pattern mode the pattern simply
    /// loops, which the clock has already done by itself.
    fn next_slot(&mut self) {
        if !self.song_mode || self.song_len == 0 {
            return;
        }
        self.song_slot = (self.song_slot + 1) % self.song_len;
        self.tune_clock();
    }

    /// Everything due on this step gets a voice.
    fn trigger_step(&mut self, step: u16) {
        let Some(pattern) = self.playing_pattern() else {
            return;
        };
        for track in 0..MAX_TRACKS {
            if !self.tracks[track].active {
                continue;
            }
            // A count, not a borrow, so the voice pool can be touched inside the loop.
            // Cloning the `Arc` is one atomic increment and the track keeps its own
            // reference, so nothing can be freed here.
            let Some(sample) = self.tracks[track].sample.clone() else {
                continue;
            };
            for i in 0..self.patterns[pattern].tracks[track].count {
                let note = self.patterns[pattern].tracks[track].notes[i];
                if note.step != step {
                    continue;
                }
                let trigger = Trigger {
                    sample: Arc::clone(&sample),
                    track: track as u16,
                    ratio: self.playback_ratio(&sample, note.pitch),
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

    // --- commands ----------------------------------------------------------

    /// Take everything waiting in the command queue. Popping is a memcpy from a ring
    /// buffer: no locks, no allocation, no chance of blocking the app thread either.
    fn drain_commands(&mut self) {
        while let Ok(command) = self.rx.pop() {
            self.apply(command);
        }
    }

    /// Notes for one track in one pattern, if both exist. Guards every index below, so a
    /// command that arrives for something that has gone is ignored rather than a panic.
    #[inline]
    fn notes_mut(&mut self, pattern: u16, track: u16) -> Option<&mut NoteList> {
        self.patterns
            .get_mut(pattern as usize)?
            .tracks
            .get_mut(track as usize)
    }

    fn apply(&mut self, command: Command) {
        match command {
            Command::SetPlaying(playing) => {
                self.playing = playing;
                if !playing {
                    // Stop leaves ringing voices to finish; it is not a panic button.
                    self.rewind_all();
                }
            }
            Command::Rewind => self.rewind_all(),
            Command::SetBpm(bpm) => self.clock.set_bpm(bpm),
            Command::SetMasterGain(gain) => self.master_gain_target = gain.clamp(0.0, 2.0),
            Command::AddTrack { track, gain } => {
                if let Some(t) = self.tracks.get_mut(track as usize) {
                    t.active = true;
                    t.muted = false;
                    t.soloed = false;
                    t.target_gain = gain.clamp(0.0, 2.0);
                    t.gain = t.target_gain;
                }
                self.forget_track(track);
            }
            Command::RemoveTrack { track } => {
                self.voices.release_track(track);
                if let Some(t) = self.tracks.get_mut(track as usize) {
                    t.active = false;
                    if let Some(sample) = t.sample.take() {
                        self.trash.put(sample);
                    }
                }
                self.forget_track(track);
            }
            Command::SetTrackSample { track, sample } => {
                self.voices.release_track(track);
                if let Some(t) = self.tracks.get_mut(track as usize) {
                    if let Some(old) = std::mem::replace(&mut t.sample, sample) {
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
            Command::SetPatternSteps { pattern, steps } => {
                let steps = steps.clamp(1, MAX_STEPS as u32);
                if let Some(p) = self.patterns.get_mut(pattern as usize) {
                    p.steps = steps;
                }
                if self.playing_pattern() == Some(pattern as usize) {
                    self.clock.set_steps(steps);
                }
            }
            Command::SetNote {
                pattern,
                track,
                note,
            } => {
                if let Some(notes) = self.notes_mut(pattern, track) {
                    notes.set(note);
                }
            }
            Command::ClearNote {
                pattern,
                track,
                step,
                pitch,
            } => {
                if let Some(notes) = self.notes_mut(pattern, track) {
                    notes.clear_one(step, pitch);
                }
            }
            Command::ClearNotes { pattern, track } => {
                if let Some(notes) = self.notes_mut(pattern, track) {
                    notes.count = 0;
                }
            }
            Command::ClearPattern { pattern } => {
                if let Some(p) = self.patterns.get_mut(pattern as usize) {
                    for notes in &mut p.tracks {
                        notes.count = 0;
                    }
                }
            }
            Command::SetActivePattern(pattern) => {
                if (pattern as usize) < self.patterns.len() {
                    self.active_pattern = pattern as usize;
                    self.tune_clock();
                }
            }
            Command::SetSongMode(on) => {
                self.song_mode = on;
                self.tune_clock();
            }
            Command::SetSongLen(len) => {
                self.song_len = (len as usize).min(MAX_SONG_SLOTS);
                if self.song_slot >= self.song_len {
                    self.song_slot = 0;
                }
                self.tune_clock();
            }
            Command::SetSongSlot { index, pattern } => {
                let index = index as usize;
                if index < MAX_SONG_SLOTS && (pattern as usize) < self.patterns.len() {
                    self.song[index] = pattern;
                    if self.song_mode && index == self.song_slot {
                        self.tune_clock();
                    }
                }
            }
            Command::SeekSong(index) => {
                if self.song_len > 0 {
                    self.song_slot = (index as usize).min(self.song_len - 1);
                }
                self.tune_clock();
                self.clock.rewind();
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
                self.rewind_all();
                self.voices.release_all();
            }
        }
    }

    /// Drop a track's notes everywhere. A deleted track must not keep playing out of a
    /// pattern nobody is looking at, and a new track in a reused slot starts empty.
    fn forget_track(&mut self, track: u16) {
        for pattern in &mut self.patterns {
            if let Some(notes) = pattern.tracks.get_mut(track as usize) {
                notes.count = 0;
            }
        }
    }
}
