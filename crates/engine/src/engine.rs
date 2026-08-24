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
use crate::model::Voicing;
use crate::plugins::{Slot, MAX_PLUGIN_NOTES};
use crate::sample::Sample;
use crate::shared::Shared;
use crate::voice::{Envelope, Trigger, VoicePool};
use crate::{
    pitch_ratio, soft_clip, velocity_gain, DEFAULT_PITCH, DEFAULT_TRACK_GAIN, MAX_BLOCK,
    MAX_NOTES_PER_TRACK, MAX_PATTERNS, MAX_SONG_STEPS, MAX_STEPS, MAX_TRACKS, PREVIEW_PATTERN,
    PREVIEW_TRACK,
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
/// A track is a sound and how that sound is played. Its notes belong to whichever pattern
/// they were drawn in, and so does how loud it is: see [`PatternState`]. What is here is the
/// same wherever the sound is used.
struct TrackState {
    /// False when the slot is free. Free slots are skipped entirely.
    active: bool,
    sample: Option<Arc<Sample>>,
    /// A CLAP instrument, for a track whose sound is a plugin rather than a file. A track has
    /// one or the other and never both: notes go to whichever is there.
    ///
    /// Boxed because everything inside it — the plugin's buffers, its event lists — is
    /// allocated on the app thread and handed over ready to use.
    plugin: Option<Box<Slot>>,
    /// The sound's shape, ready to hand to a voice: seconds already turned into per-frame
    /// steps, the pan already turned into a gain each side, the tuning already turned into a
    /// rate multiplier. Worked out when the voicing arrives, not per note.
    envelope: Envelope,
    left: f32,
    right: f32,
    tune_ratio: f64,
    level: f32,
    /// Where in the file a note starts and stops, as fractions of its length.
    start: f32,
    end: f32,
}

impl TrackState {
    fn empty(sample_rate: f64) -> Self {
        let mut track = TrackState {
            active: false,
            sample: None,
            plugin: None,
            envelope: Envelope::default(),
            left: 1.0,
            right: 1.0,
            tune_ratio: 1.0,
            level: 1.0,
            start: 0.0,
            end: 1.0,
        };
        track.voice(&Voicing::default(), sample_rate);
        track
    }

    /// Take a voicing apart into the numbers the mixing loop wants. Everything expensive
    /// about a voicing — a sine, a cosine, an exp2, four divisions — happens here, once,
    /// rather than on every note.
    fn voice(&mut self, voicing: &Voicing, sample_rate: f64) {
        let voicing = voicing.settled();
        self.envelope = Envelope::new(&voicing, sample_rate);
        // Constant power, so a sound swept across the middle does not dip in the middle.
        let angle = (voicing.pan + 1.0) * (std::f32::consts::FRAC_PI_4);
        self.left = angle.cos();
        self.right = angle.sin();
        // Both sides are 1/sqrt(2) in the middle, which would make everything quieter than
        // it was before anybody panned anything. Scale back up so dead centre is unity.
        let middle = std::f32::consts::FRAC_PI_4;
        let centre = 1.0 / middle.cos();
        self.left *= centre;
        self.right *= centre;
        self.tune_ratio = (voicing.tune as f64 / 12.0).exp2();
        self.level = voicing.level;
        self.start = voicing.start;
        self.end = voicing.end;
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
/// One pattern: how long it is, its notes, and its mixer.
///
/// The mixer is per pattern because how loud a part is, and whether you hear it at all, is
/// part of writing the part. Bitmasks for the flags because MAX_TRACKS is 32 and they are
/// read on every step.
struct PatternState {
    steps: u32,
    /// Where each fader is, in this pattern.
    gains: [f32; MAX_TRACKS],
    /// The whole pattern turned off, from the speaker on its row in the panel. Silences
    /// every track in it, wherever it plays, whatever the per-track switches say.
    hushed: bool,
    muted: u32,
    soloed: u32,
    /// The tracks played as instruments here rather than as one-shots.
    pitched: u32,
    tracks: Vec<NoteList>,
}

impl PatternState {
    /// The `Vec`s here are the point of taking `steps` in [`Engine::new`]: they are
    /// allocated once, on the app thread, and only ever indexed after that.
    fn new(steps: u32) -> Self {
        PatternState {
            steps,
            gains: [DEFAULT_TRACK_GAIN; MAX_TRACKS],
            hushed: false,
            muted: 0,
            soloed: 0,
            pitched: 0,
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
    /// Where every pattern's faders actually are, chasing where they have been put. One per
    /// pattern per track, indexed by [`Engine::fader`]: a thousand floats, allocated with
    /// everything else in `new`.
    faders: [f32; MAX_PATTERNS * MAX_TRACKS],
    /// The same, at the start of the current block, plus how much each moves per frame
    /// across it. The voice loop reads these rather than walking any state of its own.
    gains: [f32; MAX_PATTERNS * MAX_TRACKS],
    gain_incs: [f32; MAX_PATTERNS * MAX_TRACKS],
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
            tracks: std::array::from_fn(|_| TrackState::empty(sample_rate.max(1) as f64)),
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
            faders: [DEFAULT_TRACK_GAIN; MAX_PATTERNS * MAX_TRACKS],
            gains: [0.0; MAX_PATTERNS * MAX_TRACKS],
            gain_incs: [0.0; MAX_PATTERNS * MAX_TRACKS],
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

        // Every pattern has its own faders, mutes and solos, because how loud a part is and
        // whether you hear it at all is part of writing the part. A voice remembers which
        // pattern started it, so this works out a level for every pattern and track pair and
        // the voice loop picks the one it belongs to.
        //
        // Mute wins, and solo narrows what is left: if anything is soloed in a pattern then
        // only the soloed tracks are heard in it, and a muted track is silent either way.
        // Mute is the one switch that always means silence, so pressing it never has to be
        // read against what else is on.
        //
        // Gain never jumps to its new value, it slides. A block is anywhere from 64 to
        // 1024 frames, so clamping the change per block would still be a step change at
        // the block boundary — which is a zipper, or on a mute, a click. Instead each pair
        // gets a start value and a per-frame increment, and the voice loop interpolates. A
        // full-scale change always takes GAIN_SMOOTHING_FRAMES, whether it is a fader being
        // dragged or a mute coming down on something already ringing.
        let max_change = self.gain_inc * frames as f32;
        for pattern in 0..MAX_PATTERNS {
            let state = &self.patterns[pattern];
            let soloing = state.soloed != 0;
            for track in 0..MAX_TRACKS {
                let bit = 1u32 << track;
                let audible = !state.hushed
                    && self.tracks[track].active
                    && state.muted & bit == 0
                    && (!soloing || state.soloed & bit != 0);
                let target = if audible { state.gains[track] } else { 0.0 };
                let at = Self::fader(pattern, track);
                let start = self.faders[at];
                let end = start + (target - start).clamp(-max_change, max_change);
                self.faders[at] = end;
                self.gains[at] = start;
                self.gain_incs[at] = (end - start) / frames as f32;
            }
        }

        // The plugins first, because a plugin is a whole instrument's worth of sound and the
        // voices mix on top of whatever is already there.
        //
        // A plugin's level is the track's own, from the sound editor, and not any pattern's:
        // there is one sound coming out for the whole track, so there is nothing to hang a
        // per-pattern fader on. The pattern's fader is applied to the notes going in instead.
        // See the `plugins` module docs.
        let gain_inc = self.gain_inc;
        for track in self.tracks.iter_mut() {
            if let Some(slot) = track.plugin.as_mut() {
                slot.render(mix, frames, gain_inc);
            }
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

    /// Where in the fader tables a pattern and a track meet.
    #[inline]
    fn fader(pattern: usize, track: usize) -> usize {
        pattern * MAX_TRACKS + track
    }

    /// One pattern's mixer, if both it and the track exist. Guards every mixer command, so
    /// one that arrives for something that has gone is ignored rather than a panic.
    fn mixer(&mut self, pattern: u16, track: u16) -> Option<&mut PatternState> {
        if (track as usize) >= MAX_TRACKS {
            return None;
        }
        self.patterns.get_mut(pattern as usize)
    }

    /// Where one pattern's fader for one track is heading. Zero when the track is not heard
    /// in that pattern, which is what makes a mute a fade rather than a cut.
    ///
    /// Mute wins, and solo narrows what is left: anything soloed in the pattern and only the
    /// soloed tracks are heard in it, but a muted track is silent whether or not it is one of
    /// them. The block prologue works this out for every pair in one pass rather than calling
    /// this, so it can hold the pattern's state across the inner loop.
    fn target_gain(&self, pattern: usize, track: usize) -> f32 {
        let state = &self.patterns[pattern];
        let bit = 1u32 << track;
        let audible = !state.hushed
            && self.tracks[track].active
            && state.muted & bit == 0
            && (state.soloed == 0 || state.soloed & bit != 0);
        if audible {
            state.gains[track]
        } else {
            0.0
        }
    }

    /// Put a fader where it is going without sliding.
    ///
    /// Sliding is for a fader moved while it plays, so that what is already sounding comes
    /// with it. Nothing is sounding when the transport is stopped, so there is nothing to
    /// zipper — and a whole project arriving at once, which is what opening one does, would
    /// otherwise ramp every level up from the default and make the first bar loud.
    fn settle(&mut self, pattern: u16, track: u16) {
        if self.playing {
            return;
        }
        let (pattern, track) = (pattern as usize, track as usize);
        if pattern >= MAX_PATTERNS || track >= MAX_TRACKS {
            return;
        }
        self.faders[Self::fader(pattern, track)] = self.target_gain(pattern, track);
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
        // A step has gone by, so every plugin note whose length has run out ends here. First,
        // before the notes starting on this step, so playing the same key twice in a row is a
        // note off and then a note on rather than the other way round.
        for track in self.tracks.iter_mut() {
            if let Some(slot) = track.plugin.as_mut() {
                slot.step();
            }
        }

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
            if self.tracks[track].plugin.is_some() {
                self.trigger_plugin(pattern, track, step);
                continue;
            }
            // Cloning the `Arc` is one atomic increment and the track keeps its own
            // reference, so nothing can be freed here.
            let Some(sample) = self.tracks[track].sample.clone() else {
                continue;
            };
            // An instrument's notes are held for as long as they are long; a one-shot's
            // ring out, so its length is nothing to do with the sound.
            let held = self.patterns[pattern].pitched & (1 << track) != 0;
            for i in 0..self.patterns[pattern].tracks[track].count {
                let note = self.patterns[pattern].tracks[track].notes[i];
                if note.step != step {
                    continue;
                }
                // A row of boxes plays what the boxes show, and a box can only mean a note
                // at the sampler's own pitch. Anything drawn in the piano roll is still
                // there in the same lane, and comes back the moment the roll is turned on
                // again — but a note nothing on screen is showing must not make a sound.
                if !held && note.pitch != DEFAULT_PITCH {
                    continue;
                }
                let frames = if held {
                    note.length.max(1) as f64 * self.clock.samples_per_step()
                } else {
                    f64::INFINITY
                };
                let trigger = self.note_trigger(
                    track,
                    pattern as u16,
                    &sample,
                    note.pitch,
                    velocity_gain(note.velocity),
                    frames,
                );
                self.voices.trigger(trigger);
            }
        }
    }

    /// One pattern's notes at one of its own steps, on a track whose sound is a plugin.
    ///
    /// The pattern's mixer is applied here rather than to what comes out, because a plugin
    /// makes one sound for the whole track and there is nothing on the way out to apply it
    /// to. A muted pattern sends no notes; a pattern turned down sends quieter ones. That
    /// means a fader moved while a note rings does not take that note with it, which is the
    /// one place a plugin track behaves differently from a sampler one.
    fn trigger_plugin(&mut self, pattern: usize, track: usize, step: u16) {
        let state = &self.patterns[pattern];
        let bit = 1u32 << track;
        // A silenced pattern sends nothing at all, and mute wins and solo narrows over what
        // is left, exactly as they do for a sampler.
        if state.hushed || state.muted & bit != 0 || (state.soloed != 0 && state.soloed & bit == 0)
        {
            return;
        }
        let level = state.gains[track];
        let held = state.pitched & bit != 0;
        let notes = &state.tracks[track];
        // Collected up front: the notes live in `self.patterns` and the slot in `self.tracks`,
        // and both are wanted at once. Four numbers each, on the stack, never more than the
        // notes one track has on one step.
        let mut due = [(0u8, 0f32, 0u32); MAX_PLUGIN_NOTES];
        let mut count = 0;
        for i in 0..notes.count {
            let note = notes.notes[i];
            if note.step != step || count >= due.len() {
                continue;
            }
            // A row of boxes can only show notes at the sampler's own pitch, and what plays
            // has to be what you can see — the same rule the sampler follows.
            if !held && note.pitch != DEFAULT_PITCH {
                continue;
            }
            due[count] = (
                note.pitch,
                velocity_gain(note.velocity) * level,
                note.length.max(1) as u32,
            );
            count += 1;
        }
        if count == 0 {
            return;
        }
        let Some(slot) = self.tracks[track].plugin.as_mut() else {
            return;
        };
        for &(pitch, velocity, length) in &due[..count] {
            // A one-shot has no ringing out to do on a synth — there is no sample to run to
            // the end of — so it gets the shortest note there is and the plugin's own release
            // does the rest.
            let steps = if held { length } else { 1 };
            slot.note_on(pattern as u16, pitch, velocity, steps);
        }
    }

    /// Let go of every plugin note one pattern is holding, for a pattern that has just been
    /// muted or soloed out. Without this a note started before the mute would hang.
    fn hush_plugins(&mut self, pattern: u16) {
        for track in 0..MAX_TRACKS {
            let state = &self.patterns[pattern as usize];
            let bit = 1u32 << track;
            let audible = !state.hushed
                && state.muted & bit == 0
                && (state.soloed == 0 || state.soloed & bit != 0);
            if audible {
                continue;
            }
            if let Some(slot) = self.tracks[track].plugin.as_mut() {
                slot.release_pattern(pattern);
            }
        }
    }

    /// Every plugin lets go of everything. For stopping, and for the panic button.
    fn hush_all_plugins(&mut self) {
        for track in self.tracks.iter_mut() {
            if let Some(slot) = track.plugin.as_mut() {
                slot.release_all();
            }
        }
    }

    /// Source frames per output frame: the device rate correction, the note's pitch and the
    /// sound's own tuning, together.
    #[inline]
    fn playback_ratio(&self, track: usize, sample: &Sample, pitch: u8) -> f64 {
        (sample.source_rate as f64 / self.sample_rate)
            * pitch_ratio(pitch)
            * self.tracks[track].tune_ratio
    }

    /// A note on a track, with everything the track's voicing has to say about it already
    /// worked in: the envelope, where it sits, how it is tuned, its level trim and how much
    /// of the file it reads.
    ///
    /// One place, because a note from a pattern, a note played by hand and a key clicked in
    /// the piano roll all have to sound like the same instrument — and the last two used not
    /// to, which is how a trimmed sample could sound one way in the pattern and another when
    /// you clicked the row.
    fn note_trigger(
        &self,
        track: usize,
        pattern: u16,
        sample: &Arc<Sample>,
        pitch: u8,
        gain: f32,
        frames: f64,
    ) -> Trigger {
        let voicing = &self.tracks[track];
        let length = sample.frames as f64;
        Trigger {
            sample: Arc::clone(sample),
            track: track as u16,
            pattern,
            ratio: self.playback_ratio(track, sample, pitch),
            gain: gain * voicing.level,
            frames,
            envelope: voicing.envelope,
            left: voicing.left,
            right: voicing.right,
            from: length * voicing.start as f64,
            to: length * voicing.end as f64,
        }
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
                    // Stop leaves ringing voices to finish; it is not a panic button. A
                    // plugin's notes are the exception: a synth holds a note until it is told
                    // otherwise, so stopping has to say so or the last chord plays forever.
                    self.hush_all_plugins();
                    self.rewind_all();
                }
            }
            Command::Rewind => self.rewind_all(),
            Command::SetBpm(bpm) => self.clock.set_bpm(bpm),
            Command::SetMasterGain(gain) => self.master_gain_target = gain.clamp(0.0, 2.0),
            Command::AddTrack { track } => {
                if let Some(t) = self.tracks.get_mut(track as usize) {
                    t.active = true;
                }
                // A slot that has been used before starts clean: no notes, and every
                // pattern's fader for it back where a new one would be. Set rather than slid
                // to, because a track that has just appeared has no sound of its own to
                // click against and fading it in would only make its first hit quiet.
                self.forget_track(track);
            }
            Command::RemoveTrack { track } => {
                self.voices.release_track(track);
                if let Some(t) = self.tracks.get_mut(track as usize) {
                    if let Some(gone) = t.plugin.take() {
                        self.trash.put_plugin(gone);
                    }
                    t.active = false;
                    if let Some(sample) = t.sample.take() {
                        self.trash.put(sample);
                    }
                }
                self.forget_track(track);
            }
            Command::SetTrackPlugin { track, slot } => {
                if let Some(t) = self.tracks.get_mut(track as usize) {
                    // A track plays a plugin or a sample, never both. Whichever is arriving
                    // takes the other one's place.
                    if slot.is_some() {
                        if let Some(old) = t.sample.take() {
                            self.trash.put(old);
                        }
                    }
                    if let Some(mut fresh) = slot {
                        fresh.level = t.level;
                        fresh.settle();
                        if let Some(gone) = t.plugin.replace(fresh) {
                            self.trash.put_plugin(gone);
                        }
                    } else if let Some(gone) = t.plugin.take() {
                        self.trash.put_plugin(gone);
                    }
                }
            }
            Command::SetPluginParam {
                track,
                param,
                value,
            } => {
                if let Some(slot) = self
                    .tracks
                    .get_mut(track as usize)
                    .and_then(|t| t.plugin.as_mut())
                {
                    slot.set_param(param, value);
                }
            }
            Command::SetTrackVoicing { track, voicing } => {
                let rate = self.sample_rate;
                if let Some(t) = self.tracks.get_mut(track as usize) {
                    t.voice(&voicing, rate);
                    // A plugin has its own envelope, its own tuning and its own idea of where
                    // it sits, so the only part of a voicing that means anything to one is how
                    // loud it is.
                    if let Some(slot) = t.plugin.as_mut() {
                        slot.level = t.level;
                    }
                }
                // Nothing already sounding is retuned or re-enveloped part way through: a
                // voice keeps the shape it started with, so dragging the attack about while
                // it plays changes the next hit rather than warping the one you can hear.
            }
            Command::SetTrackSample { track, sample } => {
                self.voices.release_track(track);
                if let Some(t) = self.tracks.get_mut(track as usize) {
                    // A sample arriving means the track is a sampler again, so whatever
                    // plugin was on it goes home.
                    if sample.is_some() {
                        if let Some(gone) = t.plugin.take() {
                            self.trash.put_plugin(gone);
                        }
                    }
                    if let Some(old) = std::mem::replace(&mut t.sample, sample) {
                        self.trash.put(old);
                    }
                }
            }
            Command::SetPatternGain {
                pattern,
                track,
                gain,
            } => {
                if let Some(state) = self.mixer(pattern, track) {
                    state.gains[track as usize] = gain.clamp(0.0, 2.0);
                    self.settle(pattern, track);
                }
            }
            Command::SetPatternMuted {
                pattern,
                track,
                muted,
            } => {
                if let Some(state) = self.mixer(pattern, track) {
                    let bit = 1u32 << track;
                    if muted {
                        state.muted |= bit;
                    } else {
                        state.muted &= !bit;
                    }
                    self.settle(pattern, track);
                    // A sampler voice is faded out by its fader; a plugin note has to be let
                    // go of, or it hangs on through the mute.
                    self.hush_plugins(pattern);
                }
            }
            Command::MutePattern { pattern, muted } => {
                if let Some(state) = self.patterns.get_mut(pattern as usize) {
                    state.hushed = muted;
                    // Every track in it is going somewhere new, and anything a plugin is
                    // holding for this pattern has to be let go of or it hangs on through
                    // the mute — the same two things a track's own mute does.
                    for track in 0..MAX_TRACKS as u16 {
                        self.settle(pattern, track);
                    }
                    self.hush_plugins(pattern);
                }
            }
            Command::SetPatternSoloed {
                pattern,
                track,
                soloed,
            } => {
                if let Some(state) = self.mixer(pattern, track) {
                    let bit = 1u32 << track;
                    if soloed {
                        state.soloed |= bit;
                    } else {
                        state.soloed &= !bit;
                    }
                    // Solo changes what every other track in the pattern is doing too.
                    for other in 0..MAX_TRACKS as u16 {
                        self.settle(pattern, other);
                    }
                    self.hush_plugins(pattern);
                }
            }
            Command::SetPatternPitched {
                pattern,
                track,
                pitched,
            } => {
                if let Some(state) = self.mixer(pattern, track) {
                    let bit = 1u32 << track;
                    if pitched {
                        state.pitched |= bit;
                    } else {
                        state.pitched &= !bit;
                    }
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
                for track in self.tracks.iter_mut() {
                    if let Some(slot) = track.plugin.as_mut() {
                        slot.release_pattern(pattern);
                    }
                }
                if let Some(p) = self.patterns.get_mut(pattern as usize) {
                    for notes in &mut p.tracks {
                        notes.count = 0;
                    }
                    // Which tracks are instruments in it goes too, and whether the whole
                    // pattern was silenced: the app thread sends the pattern again straight
                    // after, flags and all, so nothing is left over from whatever used to be
                    // in this slot.
                    p.pitched = 0;
                    p.hushed = false;
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
                let Some(sample) = t.sample.clone() else {
                    return;
                };
                // Under no pattern's fader: clicking a row is "let me hear this sound", and
                // it should not be quiet because some pattern has it turned down. The sound's
                // own shape does apply, because that is the thing being listened to — it is
                // how the sound editor lets you hear what you are doing.
                let trigger = self.note_trigger(
                    track as usize,
                    PREVIEW_PATTERN,
                    &sample,
                    pitch,
                    velocity_gain(velocity),
                    // "Let me hear it", so it plays out whatever the track is.
                    f64::INFINITY,
                );
                self.voices.trigger(trigger);
            }
            Command::Preview { sample, gain } => {
                // Belongs to no track, so there is no voicing to apply: straight off the disk.
                let ratio = (sample.source_rate as f64 / self.sample_rate)
                    * pitch_ratio(crate::DEFAULT_PITCH);
                let to = sample.frames as f64;
                self.voices.trigger(Trigger {
                    sample,
                    track: PREVIEW_TRACK,
                    pattern: PREVIEW_PATTERN,
                    ratio,
                    gain,
                    frames: f64::INFINITY,
                    envelope: Envelope::new(&Voicing::default(), self.sample_rate),
                    left: 1.0,
                    right: 1.0,
                    from: 0.0,
                    to,
                });
            }
            Command::StopAll => {
                self.playing = false;
                self.rewind_all();
                self.voices.release_all();
                self.hush_all_plugins();
            }
        }
    }

    /// Drop a track's notes everywhere. A deleted track must not keep playing out of a
    /// pattern nobody is looking at, and a new track in a reused slot starts empty.
    fn forget_track(&mut self, track: u16) {
        if (track as usize) >= MAX_TRACKS {
            return;
        }
        let rate = self.sample_rate;
        self.tracks[track as usize].voice(&Voicing::default(), rate);
        let bit = 1u32 << track;
        for (at, pattern) in self.patterns.iter_mut().enumerate() {
            if let Some(notes) = pattern.tracks.get_mut(track as usize) {
                notes.count = 0;
            }
            pattern.gains[track as usize] = DEFAULT_TRACK_GAIN;
            pattern.muted &= !bit;
            pattern.soloed &= !bit;
            pattern.pitched &= !bit;
            // Set, not slid to: there is nothing sounding on a slot that has just been
            // claimed, so there is nothing to click.
            self.faders[Self::fader(at, track as usize)] = DEFAULT_TRACK_GAIN;
        }
    }
}
