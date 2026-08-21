//! The project. Plain data, owned by the app thread, serialised straight to `project.json`.
//!
//! Steps are stored as notes, not booleans, from day one. A step box is a note at
//! [`DEFAULT_PITCH`] one step long, so the piano roll in stage 4 is a different editor over
//! the same data rather than a file format migration.
//!
//! ## What belongs to what
//!
//! Instruments are the project's, not a pattern's. A [`Track`] is a sound plus its volume,
//! mute and solo; every pattern plays the same set of them. What a [`Pattern`] owns is the
//! notes: one [`Lane`] per track that has any. That way adding a pattern gives you a fresh
//! empty grid over the kit you already have, rather than an empty kit.
//!
//! The song is a row of bars, and each bar holds however many patterns you like. Patterns in
//! the same bar sound together, which is how a kick pattern, a hat pattern and a snare
//! pattern add up to a beat. A pattern painted across several bars keeps playing through
//! them rather than starting again every bar, so a pattern longer than a bar gets the room
//! it needs and a shorter one comes round again.

use serde::{Deserialize, Serialize};

use crate::{
    DEFAULT_PITCH, DEFAULT_STEPS, MAX_NOTES_PER_TRACK, MAX_PATTERNS, MAX_SONG_BARS, MAX_STEPS,
    MAX_TRACKS, STEPS_PER_BAR,
};

/// Bumped whenever the on-disk shape changes. Version 1 never reached a file — stage 1 had
/// no save — so there is nothing to migrate from.
pub const PROJECT_VERSION: u32 = 2;

/// A note in a pattern. `step` and `length` are in steps, `pitch` is MIDI.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Note {
    pub step: u32,
    pub pitch: u8,
    pub velocity: u8,
    pub length: u32,
}

impl Note {
    /// The note a step box means when you tick it.
    pub fn step_note(step: u32) -> Self {
        Note {
            step,
            pitch: DEFAULT_PITCH,
            velocity: 100,
            length: 1,
        }
    }
}

/// Where a track's sound comes from.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SampleRef {
    /// Relative to the project folder, e.g. `samples/kick.wav`. The file is copied in when
    /// the track is added, so a project folder is never missing a sound it uses.
    pub path: String,
    pub name: String,
}

/// One instrument: a sound and how loud it is. Belongs to the project, not to a pattern.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Track {
    /// Unique among live tracks, and also the track's slot in the audio engine.
    pub id: u16,
    pub name: String,
    pub sample: Option<SampleRef>,
    /// Linear, 0.0 to 1.5. 1.0 is unity.
    pub gain: f32,
    pub muted: bool,
    pub soloed: bool,
}

impl Track {
    pub fn new(id: u16, name: String, sample: Option<SampleRef>) -> Self {
        Track {
            id,
            name,
            sample,
            gain: 0.8,
            muted: false,
            soloed: false,
        }
    }
}

/// One track's notes inside one pattern.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct Lane {
    /// Which track these notes play on.
    pub track: u16,
    pub notes: Vec<Note>,
}

impl Lane {
    pub fn new(track: u16) -> Self {
        Lane {
            track,
            notes: Vec::new(),
        }
    }

    /// Index of the note at this step and pitch, if any.
    pub fn find(&self, step: u32, pitch: u8) -> Option<usize> {
        self.notes
            .iter()
            .position(|n| n.step == step && n.pitch == pitch)
    }

    /// True if a step box is ticked.
    pub fn has_step(&self, step: u32) -> bool {
        self.find(step, DEFAULT_PITCH).is_some()
    }

    /// Tick or untick a step box. Returns what the box now is, which may differ from what
    /// was asked for if the lane is full.
    pub fn set_step(&mut self, step: u32, on: bool) -> bool {
        match (self.find(step, DEFAULT_PITCH), on) {
            (Some(_), true) => true,
            (Some(i), false) => {
                self.notes.remove(i);
                false
            }
            (None, true) => {
                if self.notes.len() >= MAX_NOTES_PER_TRACK {
                    return false;
                }
                self.notes.push(Note::step_note(step));
                true
            }
            (None, false) => false,
        }
    }

    /// Drop notes that fall outside a shortened pattern.
    pub fn trim_to(&mut self, steps: u32) {
        self.notes.retain(|n| n.step < steps);
    }
}

/// A pattern: a name, a length in steps, and the notes played in it.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Pattern {
    /// Unique among live patterns, and also the pattern's slot in the audio engine. The
    /// song refers to patterns by this, so it survives renaming and reordering.
    pub id: u16,
    pub name: String,
    /// How many boxes a row has. Patterns in one song need not agree.
    pub steps: u32,
    pub lanes: Vec<Lane>,
}

impl Pattern {
    pub fn new(id: u16, name: String) -> Self {
        Pattern {
            id,
            name,
            steps: DEFAULT_STEPS,
            lanes: Vec::new(),
        }
    }

    pub fn lane(&self, track: u16) -> Option<&Lane> {
        self.lanes.iter().find(|l| l.track == track)
    }

    /// The lane for a track, made on the spot if the track has no notes here yet. Empty
    /// lanes are not kept around: a pattern nobody has drawn in has none at all.
    pub fn lane_mut(&mut self, track: u16) -> &mut Lane {
        if let Some(i) = self.lanes.iter().position(|l| l.track == track) {
            return &mut self.lanes[i];
        }
        self.lanes.push(Lane::new(track));
        self.lanes.last_mut().unwrap()
    }

    pub fn has_step(&self, track: u16, step: u32) -> bool {
        self.lane(track).is_some_and(|l| l.has_step(step))
    }

    /// Tick or untick a box. Returns what the box now is.
    pub fn set_step(&mut self, track: u16, step: u32, on: bool) -> bool {
        let now_on = self.lane_mut(track).set_step(step, on);
        self.lanes.retain(|l| !l.notes.is_empty());
        now_on
    }

    /// How many notes this pattern holds, across every lane.
    pub fn note_count(&self) -> usize {
        self.lanes.iter().map(|l| l.notes.len()).sum()
    }

    /// Change the length, dropping any notes that fall off the end. Returns the length
    /// actually set, which the engine and the UI both have to agree on.
    pub fn set_steps(&mut self, steps: u32) -> u32 {
        self.steps = steps.clamp(1, MAX_STEPS as u32);
        for lane in &mut self.lanes {
            lane.trim_to(self.steps);
        }
        self.lanes.retain(|l| !l.notes.is_empty());
        self.steps
    }

    /// Forget a track that has been deleted from the project.
    pub fn forget_track(&mut self, track: u16) {
        self.lanes.retain(|l| l.track != track);
    }
}

/// Everything the app knows about the song.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    pub version: u32,
    pub bpm: f32,
    /// Linear master gain before the soft clipper.
    pub master_gain: f32,
    /// The instruments. Every pattern plays all of them.
    pub tracks: Vec<Track>,
    /// Always at least one.
    pub patterns: Vec<Pattern>,
    /// The song, a bar at a time: which patterns play during each bar. Everything in a bar
    /// sounds together.
    pub song: Vec<Vec<u16>>,
}

impl Default for Project {
    fn default() -> Self {
        Project {
            version: PROJECT_VERSION,
            bpm: 120.0,
            master_gain: 0.9,
            tracks: Vec::new(),
            patterns: vec![Pattern::new(0, "Pattern 1".into())],
            song: Vec::new(),
        }
    }
}

impl Project {
    pub fn track(&self, id: u16) -> Option<&Track> {
        self.tracks.iter().find(|t| t.id == id)
    }

    pub fn track_mut(&mut self, id: u16) -> Option<&mut Track> {
        self.tracks.iter_mut().find(|t| t.id == id)
    }

    /// Lowest free engine slot, or `None` when every slot is taken.
    pub fn free_track_id(&self) -> Option<u16> {
        (0..MAX_TRACKS as u16).find(|id| self.track(*id).is_none())
    }

    /// True while any track is soloed, which is when mutes stop mattering.
    pub fn any_soloed(&self) -> bool {
        self.tracks.iter().any(|t| t.soloed)
    }

    /// Delete a track, and with it every note anyone had drawn for it.
    pub fn remove_track(&mut self, id: u16) -> Option<Track> {
        let at = self.tracks.iter().position(|t| t.id == id)?;
        for pattern in &mut self.patterns {
            pattern.forget_track(id);
        }
        Some(self.tracks.remove(at))
    }

    /// How many tracks play a sample file. Zero means the project folder can let go of it.
    pub fn sample_users(&self, path: &str) -> usize {
        self.tracks
            .iter()
            .filter(|t| t.sample.as_ref().is_some_and(|s| s.path == path))
            .count()
    }

    pub fn pattern(&self, id: u16) -> Option<&Pattern> {
        self.patterns.iter().find(|p| p.id == id)
    }

    pub fn pattern_mut(&mut self, id: u16) -> Option<&mut Pattern> {
        self.patterns.iter_mut().find(|p| p.id == id)
    }

    pub fn free_pattern_id(&self) -> Option<u16> {
        (0..MAX_PATTERNS as u16).find(|id| self.pattern(*id).is_none())
    }

    /// "Pattern 4", where 4 is the lowest number nothing is called yet. Numbering by
    /// position would rename other people's patterns behind their back.
    pub fn next_pattern_name(&self) -> String {
        (1..)
            .map(|n| format!("Pattern {n}"))
            .find(|name| !self.patterns.iter().any(|p| &p.name == name))
            .unwrap_or_else(|| "Pattern".into())
    }

    /// Add an empty pattern. `None` when the engine has no slot left for one.
    pub fn add_pattern(&mut self) -> Option<u16> {
        let id = self.free_pattern_id()?;
        let name = self.next_pattern_name();
        self.patterns.push(Pattern::new(id, name));
        Some(id)
    }

    /// Copy a pattern, notes and all, and put the copy after it in the list.
    pub fn duplicate_pattern(&mut self, id: u16) -> Option<u16> {
        let new_id = self.free_pattern_id()?;
        let at = self.patterns.iter().position(|p| p.id == id)?;
        let name = self.next_pattern_name();
        let mut copy = self.patterns[at].clone();
        copy.id = new_id;
        copy.name = name;
        self.patterns.insert(at + 1, copy);
        Some(new_id)
    }

    /// Delete a pattern and take it out of the song. Refuses to delete the last one: an
    /// editor with nothing to edit is a dead end.
    pub fn remove_pattern(&mut self, id: u16) -> bool {
        if self.patterns.len() <= 1 {
            return false;
        }
        let Some(at) = self.patterns.iter().position(|p| p.id == id) else {
            return false;
        };
        self.patterns.remove(at);
        for bar in &mut self.song {
            bar.retain(|in_bar| *in_bar != id);
        }
        self.trim_song();
        true
    }

    /// The patterns playing in a bar. Bars past the end of the song are silent.
    pub fn bar(&self, index: usize) -> &[u16] {
        self.song
            .get(index)
            .map(|bar| bar.as_slice())
            .unwrap_or(&[])
    }

    pub fn bar_has(&self, index: usize, pattern: u16) -> bool {
        self.bar(index).contains(&pattern)
    }

    /// The patterns in a bar as one bit each, which is how the audio thread holds them.
    pub fn bar_mask(&self, index: usize) -> u32 {
        self.bar(index)
            .iter()
            .filter(|id| (**id as usize) < MAX_PATTERNS)
            .fold(0u32, |mask, id| mask | (1 << *id))
    }

    pub fn bars(&self) -> usize {
        self.song.len()
    }

    /// Put a pattern in a bar of the song, or take it out. Returns whether it is in there
    /// now, which is false if the pattern or the bar does not exist.
    ///
    /// Turning one on fills in any empty bars before it, so you can drop a pattern in at
    /// bar twelve and get eleven bars of silence in front of it. Turning one off leaves the
    /// bar where it is, empty, because a rest in the middle of a song is a real thing to
    /// want — only empty bars on the end are dropped.
    pub fn set_bar_pattern(&mut self, index: usize, pattern: u16, on: bool) -> bool {
        if self.pattern(pattern).is_none() {
            return false;
        }
        if !on {
            if index < self.song.len() {
                self.song[index].retain(|id| *id != pattern);
                self.trim_song();
            }
            return false;
        }
        if index >= MAX_SONG_BARS {
            return false;
        }
        while self.song.len() <= index {
            self.song.push(Vec::new());
        }
        let bar = &mut self.song[index];
        if !bar.contains(&pattern) {
            bar.push(pattern);
            // Sorted, so the same song is always written out the same way.
            bar.sort_unstable();
        }
        true
    }

    /// Take a bar out of the song altogether, so everything after it moves up.
    pub fn remove_bar(&mut self, index: usize) -> bool {
        if index >= self.song.len() {
            return false;
        }
        self.song.remove(index);
        self.trim_song();
        true
    }

    /// Empty bars on the end are not part of the song.
    fn trim_song(&mut self) {
        while self.song.last().is_some_and(|bar| bar.is_empty()) {
            self.song.pop();
        }
    }

    /// Steps in the whole song, for drawing it.
    pub fn song_steps(&self) -> u32 {
        self.song.len() as u32 * STEPS_PER_BAR
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kit() -> Project {
        let mut project = Project::default();
        project.tracks.push(Track::new(0, "kick".into(), None));
        project.tracks.push(Track::new(1, "snare".into(), None));
        project
    }

    #[test]
    fn a_step_box_is_a_note() {
        let mut pattern = Pattern::new(0, "p".into());
        assert!(!pattern.has_step(0, 4));
        assert!(pattern.set_step(0, 4, true));
        assert!(pattern.has_step(0, 4));
        // Stored as a note at the sampler's unity pitch, which is what makes the piano
        // roll in stage 4 a different editor rather than a file format migration.
        assert_eq!(pattern.lane(0).unwrap().notes[0], Note::step_note(4));
        assert!(!pattern.set_step(0, 4, false));
        assert_eq!(pattern.note_count(), 0);
        // And the empty lane goes with it, so a pattern nobody drew in stays empty.
        assert!(pattern.lanes.is_empty());
    }

    #[test]
    fn ticking_a_ticked_box_changes_nothing() {
        let mut pattern = Pattern::new(0, "p".into());
        pattern.set_step(0, 2, true);
        pattern.set_step(0, 2, true);
        assert_eq!(pattern.note_count(), 1);
    }

    #[test]
    fn a_lane_will_not_grow_past_what_the_engine_holds() {
        let mut pattern = Pattern::new(0, "p".into());
        for step in 0..(MAX_NOTES_PER_TRACK as u32 + 20) {
            pattern.set_step(0, step, true);
        }
        assert_eq!(pattern.note_count(), MAX_NOTES_PER_TRACK);
    }

    #[test]
    fn shortening_a_pattern_drops_the_notes_that_fall_off() {
        let mut pattern = Pattern::new(0, "p".into());
        for step in 0..16 {
            pattern.set_step(0, step, true);
        }
        assert_eq!(pattern.set_steps(8), 8);
        assert_eq!(pattern.note_count(), 8);
        assert!(pattern.lane(0).unwrap().notes.iter().all(|n| n.step < 8));
    }

    #[test]
    fn pattern_length_is_capped_at_what_the_engine_plays() {
        let mut pattern = Pattern::new(0, "p".into());
        assert_eq!(pattern.set_steps(9_000), MAX_STEPS as u32);
        assert_eq!(pattern.set_steps(0), 1);
    }

    #[test]
    fn patterns_hold_their_own_notes() {
        let mut project = kit();
        let second = project.add_pattern().unwrap();
        project.pattern_mut(0).unwrap().set_step(0, 0, true);
        project.pattern_mut(second).unwrap().set_step(0, 8, true);

        assert!(project.pattern(0).unwrap().has_step(0, 0));
        assert!(!project.pattern(0).unwrap().has_step(0, 8));
        assert!(project.pattern(second).unwrap().has_step(0, 8));
        // The instruments are the project's, so both patterns play the same kit.
        assert_eq!(project.tracks.len(), 2);
    }

    #[test]
    fn track_ids_are_the_lowest_free_engine_slot() {
        let mut project = Project::default();
        assert_eq!(project.free_track_id(), Some(0));
        project.tracks.push(Track::new(0, "a".into(), None));
        project.tracks.push(Track::new(1, "b".into(), None));
        assert_eq!(project.free_track_id(), Some(2));

        // Deleting the middle one frees its slot for the next track, so the engine never
        // has to shuffle its state when a row is removed.
        project.remove_track(0);
        assert_eq!(project.free_track_id(), Some(0));
    }

    #[test]
    fn slots_run_out_rather_than_overflowing() {
        let mut project = Project::default();
        for id in 0..MAX_TRACKS as u16 {
            project.tracks.push(Track::new(id, "x".into(), None));
        }
        assert_eq!(project.free_track_id(), None);

        while project.add_pattern().is_some() {}
        assert_eq!(project.patterns.len(), MAX_PATTERNS);
    }

    #[test]
    fn deleting_a_track_takes_its_notes_with_it() {
        let mut project = kit();
        let second = project.add_pattern().unwrap();
        for id in [0, 1] {
            project.pattern_mut(0).unwrap().set_step(id, 0, true);
            project.pattern_mut(second).unwrap().set_step(id, 4, true);
        }

        project.remove_track(1);
        assert!(project.track(1).is_none());
        assert_eq!(project.pattern(0).unwrap().note_count(), 1);
        assert_eq!(project.pattern(second).unwrap().note_count(), 1);
        assert!(project.pattern(0).unwrap().lane(1).is_none());
    }

    #[test]
    fn new_patterns_are_named_after_the_lowest_free_number() {
        let mut project = Project::default();
        let second = project.add_pattern().unwrap();
        assert_eq!(project.pattern(second).unwrap().name, "Pattern 2");
        project.pattern_mut(second).unwrap().name = "Chorus".into();
        let third = project.add_pattern().unwrap();
        assert_eq!(project.pattern(third).unwrap().name, "Pattern 2");
    }

    #[test]
    fn duplicating_copies_the_notes_and_lands_next_to_the_original() {
        let mut project = kit();
        project.pattern_mut(0).unwrap().set_step(1, 6, true);
        project.add_pattern().unwrap();

        let copy = project.duplicate_pattern(0).unwrap();
        assert_eq!(
            project.patterns[1].id, copy,
            "the copy goes right after the original"
        );
        assert!(project.pattern(copy).unwrap().has_step(1, 6));

        // A copy is its own pattern: drawing in it leaves the original alone.
        project.pattern_mut(copy).unwrap().set_step(1, 7, true);
        assert!(!project.pattern(0).unwrap().has_step(1, 7));
    }

    #[test]
    fn a_bar_of_the_song_holds_as_many_patterns_as_you_like() {
        let mut project = Project::default();
        let hats = project.add_pattern().unwrap();
        let snare = project.add_pattern().unwrap();

        // A kick, a hat and a snare pattern, all sounding together in the first bar.
        assert!(project.set_bar_pattern(0, 0, true));
        assert!(project.set_bar_pattern(0, hats, true));
        assert!(project.set_bar_pattern(0, snare, true));
        assert_eq!(project.bar(0), &[0, hats, snare]);
        assert_eq!(project.bars(), 1);
        assert_eq!(project.song_steps(), STEPS_PER_BAR);

        // Taking one out leaves the others where they are.
        assert!(!project.set_bar_pattern(0, hats, false));
        assert_eq!(project.bar(0), &[0, snare]);
        assert!(project.bar_has(0, snare));
        assert!(!project.bar_has(0, hats));
    }

    #[test]
    fn the_bar_mask_is_what_the_audio_thread_gets() {
        let mut project = Project::default();
        let second = project.add_pattern().unwrap();
        project.set_bar_pattern(0, 0, true);
        project.set_bar_pattern(0, second, true);
        assert_eq!(project.bar_mask(0), 0b11);
        assert_eq!(project.bar_mask(1), 0, "a bar past the end is silent");
    }

    #[test]
    fn a_pattern_dropped_in_late_gets_silence_in_front_of_it() {
        let mut project = Project::default();
        assert!(project.set_bar_pattern(3, 0, true));
        assert_eq!(project.bars(), 4);
        assert!(project.bar(0).is_empty());
        assert!(project.bar_has(3, 0));

        // A rest in the middle stays a rest; only empty bars on the end go.
        project.set_bar_pattern(1, 0, true);
        project.set_bar_pattern(1, 0, false);
        assert_eq!(project.bars(), 4);
        project.set_bar_pattern(3, 0, false);
        assert_eq!(project.bars(), 0, "nothing left, so no song");
    }

    #[test]
    fn a_bar_can_be_taken_out_to_close_a_gap() {
        let mut project = Project::default();
        let second = project.add_pattern().unwrap();
        project.set_bar_pattern(0, 0, true);
        project.set_bar_pattern(2, second, true);

        assert!(project.remove_bar(1));
        assert_eq!(project.bars(), 2);
        assert!(project.bar_has(1, second));
        assert!(!project.remove_bar(9));
    }

    #[test]
    fn the_song_will_not_hold_a_pattern_that_does_not_exist() {
        let mut project = Project::default();
        assert!(!project.set_bar_pattern(0, 7, true));
        assert!(project.song.is_empty());
    }

    #[test]
    fn deleting_a_pattern_takes_it_out_of_the_song() {
        let mut project = Project::default();
        let second = project.add_pattern().unwrap();
        project.set_bar_pattern(0, 0, true);
        project.set_bar_pattern(0, second, true);
        project.set_bar_pattern(1, second, true);

        assert!(project.remove_pattern(second));
        assert_eq!(project.bar(0), &[0]);
        // Bar 1 held nothing else, so the song is one bar long again.
        assert_eq!(project.bars(), 1);

        // The last pattern stays put, whatever anyone asks.
        assert!(!project.remove_pattern(0));
        assert_eq!(project.patterns.len(), 1);
    }

    #[test]
    fn a_sample_is_shared_until_the_last_track_using_it_goes() {
        let mut project = Project::default();
        for id in 0..2 {
            project.tracks.push(Track::new(
                id,
                "kick".into(),
                Some(SampleRef {
                    path: "samples/kick.wav".into(),
                    name: "kick".into(),
                }),
            ));
        }
        assert_eq!(project.sample_users("samples/kick.wav"), 2);
        project.remove_track(0);
        assert_eq!(project.sample_users("samples/kick.wav"), 1);
        project.remove_track(1);
        assert_eq!(project.sample_users("samples/kick.wav"), 0);
    }

    #[test]
    fn survives_a_round_trip_through_json() {
        let mut project = Project {
            bpm: 138.0,
            ..Default::default()
        };
        project.tracks.push(Track::new(
            0,
            "kick".into(),
            Some(SampleRef {
                path: "samples/kick.wav".into(),
                name: "kick".into(),
            }),
        ));
        let second = project.add_pattern().unwrap();
        project.pattern_mut(second).unwrap().set_steps(32);
        project.pattern_mut(0).unwrap().set_step(0, 0, true);
        project.pattern_mut(second).unwrap().set_step(0, 8, true);
        project.set_bar_pattern(0, 0, true);
        project.set_bar_pattern(0, second, true);
        project.set_bar_pattern(1, second, true);

        let json = serde_json::to_string(&project).unwrap();
        let back: Project = serde_json::from_str(&json).unwrap();
        assert_eq!(back.bpm, 138.0);
        assert_eq!(back.version, PROJECT_VERSION);
        assert_eq!(back.tracks.len(), 1);
        assert_eq!(
            back.tracks[0].sample.as_ref().unwrap().path,
            "samples/kick.wav"
        );
        assert_eq!(back.patterns.len(), 2);
        assert_eq!(back.pattern(second).unwrap().steps, 32);
        assert!(back.pattern(second).unwrap().has_step(0, 8));
        assert_eq!(back.song, vec![vec![0, second], vec![second]]);
    }
}
