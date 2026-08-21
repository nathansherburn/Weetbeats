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
//! The song is a list of placements: a pattern, and the step it starts at. One placement is
//! one play-through, so a four step pattern takes four steps of the song and a thirty two
//! step pattern takes thirty two. Placements sit on multiples of their own pattern's length,
//! which is the only grid that makes sense when patterns are different lengths, and any
//! number of them can overlap: that is how a kick pattern, a hat pattern and a snare pattern
//! add up to a beat.

use serde::{Deserialize, Serialize};

use crate::{
    DEFAULT_PITCH, DEFAULT_STEPS, MAX_NOTES_PER_TRACK, MAX_PATTERNS, MAX_PLACEMENTS,
    MAX_SONG_STEPS, MAX_STEPS, MAX_TRACKS, STEPS_PER_BAR,
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

/// One pattern in the song: which pattern, and the step it starts at.
///
/// `step` is always a multiple of that pattern's own length, so two placements of the same
/// pattern can never overlap each other and a placement is always exactly one play-through.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "camelCase")]
pub struct Placement {
    pub step: u32,
    pub pattern: u16,
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
    /// The song: what plays where. Sorted, so the same song is always written out the same
    /// way. Placements overlap freely — that is the point of them.
    pub song: Vec<Placement>,
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
        self.song.retain(|placement| placement.pattern != id);
        true
    }

    /// True if this pattern plays at this step of the song.
    pub fn placed(&self, pattern: u16, step: u32) -> bool {
        self.song
            .iter()
            .any(|p| p.pattern == pattern && p.step == step)
    }

    /// Where a pattern's `slot`th place in the song would start. Slots are as long as the
    /// pattern, so slot 3 of a four step pattern starts at step 12 and slot 3 of a sixteen
    /// step pattern starts at step 48.
    pub fn slot_step(&self, pattern: u16, slot: u32) -> Option<u32> {
        let steps = self.pattern(pattern)?.steps.max(1);
        Some(slot.saturating_mul(steps))
    }

    /// Put a pattern in the song, or take it out. `slot` counts in the pattern's own
    /// lengths, so one slot is one play-through of it.
    ///
    /// Returns whether it is in there now, which is false if the pattern does not exist, the
    /// song is full, or it would run off the end of the longest song we hold.
    pub fn set_placement(&mut self, pattern: u16, slot: u32, on: bool) -> bool {
        let Some(step) = self.slot_step(pattern, slot) else {
            return false;
        };
        let steps = self.pattern(pattern).map(|p| p.steps).unwrap_or(0);
        if !on {
            self.song
                .retain(|p| !(p.pattern == pattern && p.step == step));
            return false;
        }
        if step + steps > MAX_SONG_STEPS as u32 || self.song.len() >= MAX_PLACEMENTS {
            return false;
        }
        if !self.placed(pattern, step) {
            self.song.push(Placement { step, pattern });
            self.song.sort_unstable();
        }
        true
    }

    /// Everything that starts inside a bar, gone. What right clicking a bar does: the way
    /// out of a mess without having to pick the pieces off one at a time.
    pub fn clear_bar(&mut self, bar: u32) -> usize {
        let from = bar * STEPS_PER_BAR;
        let to = from + STEPS_PER_BAR;
        let before = self.song.len();
        self.song.retain(|p| p.step < from || p.step >= to);
        before - self.song.len()
    }

    /// Where the song ends, rounded up to a whole bar so it loops somewhere musical.
    pub fn song_steps(&self) -> u32 {
        let end = self
            .song
            .iter()
            .map(|p| p.step + self.pattern(p.pattern).map(|one| one.steps).unwrap_or(0))
            .max()
            .unwrap_or(0);
        end.div_ceil(STEPS_PER_BAR) * STEPS_PER_BAR
    }

    pub fn song_bars(&self) -> u32 {
        self.song_steps() / STEPS_PER_BAR
    }

    /// Change a pattern's length, and move its places in the song onto the new grid.
    ///
    /// A pattern that was four steps and is now sixteen cannot keep four placements a bar
    /// apart: they would sit on top of each other. So they are snapped to the new length and
    /// the ones that land on the same step become one.
    pub fn set_pattern_steps(&mut self, id: u16, steps: u32) -> u32 {
        let Some(pattern) = self.pattern_mut(id) else {
            return 0;
        };
        let steps = pattern.set_steps(steps);
        for placement in &mut self.song {
            if placement.pattern == id {
                placement.step = (placement.step / steps) * steps;
            }
        }
        self.song.sort_unstable();
        self.song.dedup();
        steps
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
    fn patterns_overlap_in_the_song_so_they_can_play_together() {
        let mut project = Project::default();
        let hats = project.add_pattern().unwrap();
        let snare = project.add_pattern().unwrap();

        // A kick, a hat and a snare pattern, all starting at the top of the song.
        assert!(project.set_placement(0, 0, true));
        assert!(project.set_placement(hats, 0, true));
        assert!(project.set_placement(snare, 0, true));
        assert_eq!(project.song.len(), 3);
        assert!(project.placed(hats, 0));
        assert_eq!(project.song_steps(), STEPS_PER_BAR);

        // Taking one out leaves the others where they are.
        assert!(!project.set_placement(hats, 0, false));
        assert!(!project.placed(hats, 0));
        assert!(project.placed(snare, 0));
    }

    /// The point of placing by the pattern's own length: a four step pattern takes four steps
    /// of the song, not a whole bar of it.
    #[test]
    fn a_slot_is_as_long_as_the_pattern_in_it() {
        let mut project = Project::default();
        let short = project.add_pattern().unwrap();
        project.set_pattern_steps(short, 4);

        assert_eq!(project.slot_step(short, 0), Some(0));
        assert_eq!(project.slot_step(short, 1), Some(4));
        assert_eq!(project.slot_step(0, 1), Some(16));

        // One four step pattern in the first slot: four steps of music, one bar of song.
        project.set_placement(short, 0, true);
        assert!(project.placed(short, 0));
        assert!(
            !project.placed(short, 4),
            "one slot is one play through, not a bar of them"
        );
        assert_eq!(
            project.song_steps(),
            STEPS_PER_BAR,
            "the song still loops on the bar"
        );

        // And the next slot along starts where the first one ended.
        project.set_placement(short, 1, true);
        assert!(project.placed(short, 4));
        assert_eq!(project.song.len(), 2);
    }

    #[test]
    fn the_song_is_as_long_as_the_last_thing_in_it_rounded_up_to_a_bar() {
        let mut project = Project::default();
        let long = project.add_pattern().unwrap();
        project.set_pattern_steps(long, 32);

        project.set_placement(0, 2, true); // sixteen steps, at step 32
        assert_eq!(project.song_steps(), 48);
        project.set_placement(long, 2, true); // thirty two steps, at step 64
        assert_eq!(project.song_steps(), 96);
        assert_eq!(project.song_bars(), 6);
    }

    #[test]
    fn a_pattern_cannot_be_placed_twice_in_the_same_slot() {
        let mut project = Project::default();
        assert!(project.set_placement(0, 3, true));
        assert!(project.set_placement(0, 3, true));
        assert_eq!(project.song.len(), 1);
    }

    #[test]
    fn clearing_a_bar_takes_out_everything_that_starts_in_it() {
        let mut project = Project::default();
        let short = project.add_pattern().unwrap();
        project.set_pattern_steps(short, 4);
        // Four short placements across the first bar, and one in the second.
        for slot in 0..5 {
            project.set_placement(short, slot, true);
        }
        project.set_placement(0, 0, true);

        assert_eq!(project.clear_bar(0), 5, "four short ones and the long one");
        assert_eq!(project.song.len(), 1);
        assert!(project.placed(short, 16));
    }

    #[test]
    fn changing_a_length_moves_the_places_it_was_put() {
        let mut project = Project::default();
        project.set_pattern_steps(0, 4);
        for slot in 0..4 {
            project.set_placement(0, slot, true);
        }
        assert_eq!(project.song.len(), 4);

        // Four steps to sixteen: all four placements were inside one bar, so they become one.
        assert_eq!(project.set_pattern_steps(0, 16), 16);
        assert_eq!(project.song.len(), 1);
        assert!(project.placed(0, 0));
    }

    #[test]
    fn the_song_will_not_hold_a_pattern_that_does_not_exist() {
        let mut project = Project::default();
        assert!(!project.set_placement(7, 0, true));
        assert!(project.song.is_empty());
    }

    #[test]
    fn the_song_has_an_end_to_it() {
        let mut project = Project::default();
        // A slot way past the longest song we hold is refused rather than wrapped.
        assert!(!project.set_placement(0, 100_000, true));
        assert!(project.song.is_empty());
    }

    #[test]
    fn deleting_a_pattern_takes_it_out_of_the_song() {
        let mut project = Project::default();
        let second = project.add_pattern().unwrap();
        project.set_placement(0, 0, true);
        project.set_placement(second, 0, true);
        project.set_placement(second, 1, true);

        assert!(project.remove_pattern(second));
        assert_eq!(project.song.len(), 1);
        assert!(project.placed(0, 0));

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
        project.set_pattern_steps(second, 32);
        project.pattern_mut(0).unwrap().set_step(0, 0, true);
        project.pattern_mut(second).unwrap().set_step(0, 8, true);
        project.set_placement(0, 0, true);
        project.set_placement(second, 0, true);
        project.set_placement(second, 1, true);

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
        assert_eq!(back.song.len(), 3);
        assert!(
            back.placed(second, 32),
            "a thirty two step pattern in its second slot"
        );
    }
}
