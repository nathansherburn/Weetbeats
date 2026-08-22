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
//! pattern editor. In song mode the clock counts the whole song, and the engine holds a slot
//! per step saying which patterns *start* there. Any number can start at the same step and
//! they all sound together, which is how a kick pattern, a hat pattern and a snare pattern
//! add up to a beat.
//!
//! Each pattern then carries how far through it is and how much of it is left, so a placement
//! plays once through and stops, however long the pattern is and whatever else is going on
//! around it.

use std::sync::Arc;

use rtrb::Consumer;

use crate::clock::StepClock;
use crate::command::{Command, EngineNote, TrashBin};
use crate::sample::Sample;
use crate::shared::Shared;
use crate::voice::{Trigger, VoicePool};
use crate::{
    pitch_ratio, soft_clip, velocity_gain, MAX_BLOCK, MAX_NOTES_PER_TRACK, MAX_PATTERNS,
    MAX_SONG_STEPS, MAX_STEPS, MAX_TRACKS, PREVIEW_TRACK,
};

/// How fast a gain change slides to its new value. About 10ms at 48k, which is slow enough
/// to have no zipper noise and fast enough that a slider feels connected.
const GAIN_SMOOTHING_FRAMES: f32 = 480.0;

/// How fast the level meter falls, in full scale per second.
///
/// Without this the meter would be whatever the last callback happened to peak at, and the
/// front end reads it about sixty times a second while callbacks come three times as often:
/// two out of three peaks would never be seen, so a drum hit mostly would not register. It
/// holds instead, and slides down.
const METER_FALL_PER_SECOND: f32 = 1.6;

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
    /// A sampler instrument rather than a one-shot: its notes stop when they end.
    pitched: bool,
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
            pitched: false,
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
    /// The song: which patterns start at each step, one bit each. Allocated once, in `new`,
    /// and only ever indexed after that.
    starts: Vec<u32>,
    /// How long the block starting at each step is, per pattern, in steps. `starts` says a
    /// block begins; this says how much song it fills. A quarter of a megabyte, allocated
    /// once in `new` alongside `starts`.
    lengths: Vec<u16>,
    /// Steps of the song in use.
    song_len: u32,
    /// How many steps into its block each pattern is, and how many it has left before the
    /// block is over. A block longer than its pattern wraps round it; a shorter one stops
    /// part way through.
    run_step: [u32; MAX_PATTERNS],
    run_left: [u32; MAX_PATTERNS],
    /// What sounded on the last step, one bit per pattern, for the UI.
    sounding: u32,
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
    /// The level the meter is showing, which falls rather than dropping to whatever the
    /// last callback did.
    peak_held: f32,
    meter_fall: f32,
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
            starts: vec![0; MAX_SONG_STEPS],
            lengths: vec![0; MAX_SONG_STEPS * MAX_PATTERNS],
            song_len: 0,
            run_step: [0; MAX_PATTERNS],
            run_left: [0; MAX_PATTERNS],
            sounding: 0,
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
            peak_held: 0.0,
            meter_fall: METER_FALL_PER_SECOND / sample_rate.max(1) as f32,
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
                    // Round again at the top of the song, where nothing is half played.
                    self.resync_runs();
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
        self.shared
            .set_position(self.clock.step(), self.clock.progress(), self.sounding);
        // Hold the loudest thing that happened and let it slide down, so a hit that lands
        // between two of the front end's polls is still seen.
        self.peak_held = (self.peak_held - self.meter_fall * total_frames as f32).max(peak);
        self.shared
            .set_meters(self.voices.active(), self.peak_held.max(0.0));
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

    /// How long the thing being played is, in steps. The whole song in song mode, so the
    /// clock's step *is* the song position and coming round to the top is the clock's job.
    fn playing_steps(&self) -> u32 {
        if self.song_mode {
            self.song_len.max(1)
        } else {
            self.patterns[self.active_pattern].steps
        }
    }

    /// Point the clock at whatever is playing now. A shorter pattern pulls the playhead
    /// back to the top rather than leaving it past the end.
    fn tune_clock(&mut self) {
        let steps = self.playing_steps();
        self.clock.set_steps(steps);
    }

    /// Where in the `lengths` grid a step and a pattern meet.
    fn slot(step: u32, pattern: usize) -> usize {
        step as usize * MAX_PATTERNS + pattern
    }

    /// Work out where each pattern is up to, given where the playhead is.
    ///
    /// Blocks of one pattern never overlap, so the nearest start behind us is the only one
    /// that could still be sounding: if its length has run out, no earlier one is any
    /// better. Walking back looks at all thirty two patterns at once and stops as soon as
    /// every one has been accounted for. Only needed when the playhead jumps — a seek, a
    /// stop, the top of the song — never while it is simply playing on.
    fn resync_runs(&mut self) {
        self.run_step = [0; MAX_PATTERNS];
        self.run_left = [0; MAX_PATTERNS];
        if !self.song_mode {
            return;
        }
        let now = self.clock.step();
        let mut looking = u32::MAX;
        for back in 0..=now {
            let found = self.starts[(now - back) as usize] & looking;
            if found == 0 {
                continue;
            }
            for pattern in 0..MAX_PATTERNS {
                if found & (1u32 << pattern) == 0 {
                    continue;
                }
                let length = self.lengths[Self::slot(now - back, pattern)] as u32;
                if back < length {
                    self.run_step[pattern] = back;
                    self.run_left[pattern] = length - back;
                }
            }
            looking &= !found;
            if looking == 0 {
                break;
            }
        }
    }

    /// Back to the top: the first step of the song, or of the pattern.
    fn rewind_all(&mut self) {
        self.tune_clock();
        self.clock.rewind();
        self.resync_runs();
    }

    /// Everything due on this step gets a voice.
    ///
    /// `step` is the step of the song in song mode and the step of the pattern otherwise.
    fn trigger_step(&mut self, step: u16) {
        if !self.song_mode {
            self.sounding = 1u32 << self.active_pattern;
            self.trigger_pattern(self.active_pattern, step);
            return;
        }

        let starting = self.starts.get(step as usize).copied().unwrap_or(0);
        self.sounding = 0;
        for pattern in 0..MAX_PATTERNS {
            if starting & (1u32 << pattern) != 0 {
                // A block begins here, so the pattern starts from its own first step.
                self.run_step[pattern] = 0;
                self.run_left[pattern] = self.lengths[Self::slot(step as u32, pattern)] as u32;
            }
            if self.run_left[pattern] == 0 {
                continue;
            }
            self.sounding |= 1u32 << pattern;
            // A block longer than its pattern comes round again rather than going quiet.
            let at = (self.run_step[pattern] % self.patterns[pattern].steps.max(1)) as u16;
            self.trigger_pattern(pattern, at);
            self.run_step[pattern] += 1;
            self.run_left[pattern] -= 1;
        }
    }

    /// One pattern's notes at one of its own steps.
    fn trigger_pattern(&mut self, pattern: usize, step: u16) {
        for track in 0..MAX_TRACKS {
            if !self.tracks[track].active {
                continue;
            }
            // Cloning the `Arc` is one atomic increment and the track keeps its own
            // reference, so nothing can be freed here.
            let Some(sample) = self.tracks[track].sample.clone() else {
                continue;
            };
            // An instrument's notes are held for as long as they are long; a one-shot's
            // ring out, so its length is nothing to do with the sound.
            let held = self.tracks[track].pitched;
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
                    frames: if held {
                        note.length.max(1) as f64 * self.clock.samples_per_step()
                    } else {
                        f64::INFINITY
                    },
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
                    t.pitched = false;
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
            Command::SetTrackPitched { track, pitched } => {
                if let Some(t) = self.tracks.get_mut(track as usize) {
                    t.pitched = pitched;
                }
            }
            Command::SetPatternSteps { pattern, steps } => {
                let steps = steps.clamp(1, MAX_STEPS as u32);
                if let Some(p) = self.patterns.get_mut(pattern as usize) {
                    p.steps = steps;
                }
                // The song's length comes from the app thread, which knows where every
                // placement now sits; only the pattern the editor is looping is our business.
                if !self.song_mode && self.active_pattern == pattern as usize {
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
                self.clock.rewind();
                self.resync_runs();
            }
            Command::SetSongLen(len) => {
                self.song_len = len.min(MAX_SONG_STEPS as u32);
                self.tune_clock();
            }
            Command::ClearSong => {
                self.starts.fill(0);
                self.lengths.fill(0);
                self.song_len = 0;
                for pattern in 0..MAX_PATTERNS {
                    self.run_left[pattern] = 0;
                }
                self.tune_clock();
            }
            Command::PlacePattern {
                pattern,
                step,
                length,
            } => {
                // Editing the song while it plays does not restart anything: whatever is
                // sounding keeps its place, which is what you want while you paint.
                let pattern = pattern as usize;
                if pattern < MAX_PATTERNS && (step as usize) < MAX_SONG_STEPS {
                    self.starts[step as usize] |= 1u32 << pattern;
                    self.lengths[Self::slot(step, pattern)] =
                        length.clamp(1, MAX_SONG_STEPS as u32) as u16;
                }
            }
            Command::UnplacePattern { pattern, step } => {
                let pattern = pattern as usize;
                if pattern < MAX_PATTERNS && (step as usize) < MAX_SONG_STEPS {
                    self.starts[step as usize] &= !(1u32 << pattern);
                    self.lengths[Self::slot(step, pattern)] = 0;
                }
            }
            Command::SeekSong(step) => {
                self.tune_clock();
                self.clock.jump_to(step);
                self.resync_runs();
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
                    // Clicking a row is "let me hear it", so it plays out whatever the track is.
                    frames: f64::INFINITY,
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
                    frames: f64::INFINITY,
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
