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
    /// Left over from when being an instrument belonged to the track rather than to each
    /// pattern. Only read on the way in, by [`Project::repair`], which marks every pattern
    /// for a track that was pitched and then clears this — so an old project still sounds
    /// the way it did. Never written by this version.
    #[serde(default, skip_serializing_if = "not")]
    pub pitched: bool,
}

fn not(flag: &bool) -> bool {
    !*flag
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
            pitched: false,
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

    /// Add a note, or replace the one already at its step and pitch. False when the lane is
    /// as full as the engine will hold.
    pub fn set_note(&mut self, note: Note) -> bool {
        match self.find(note.step, note.pitch) {
            Some(i) => {
                self.notes[i] = note;
                true
            }
            None => {
                if self.notes.len() >= MAX_NOTES_PER_TRACK {
                    return false;
                }
                self.notes.push(note);
                true
            }
        }
    }

    /// Take out the note at a step and pitch, if there is one.
    pub fn clear_note(&mut self, step: u32, pitch: u8) -> bool {
        match self.find(step, pitch) {
            Some(i) => {
                self.notes.remove(i);
                true
            }
            None => false,
        }
    }

    /// The note at a step and pitch.
    pub fn note(&self, step: u32, pitch: u8) -> Option<Note> {
        self.find(step, pitch).map(|i| self.notes[i])
    }

    /// Drop notes that fall outside a shortened pattern.
    ///
    /// A note that starts inside the pattern but runs off the end is shortened rather than
    /// dropped: it is still a note you drew, it just has less room now.
    pub fn trim_to(&mut self, steps: u32) {
        self.notes.retain(|n| n.step < steps);
        for note in &mut self.notes {
            note.length = note.length.min(steps - note.step).max(1);
        }
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
    /// Which of the song view's colours its blocks are. `None` means nobody chose, and the
    /// front end picks from the pattern's id so a new pattern looks different from the last.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub colour: Option<u8>,
    /// The tracks played as instruments *in this pattern*: pitched across the keyboard, with
    /// each note stopping when it ends, and edited as a piano roll rather than a row of
    /// boxes. Everything else is a one-shot here — hit it and the whole sample plays.
    ///
    /// Per pattern rather than per track, because it is a decision about the part, not about
    /// the sound: the same bass can be a row of boxes holding down a rhythm in one pattern
    /// and a melody in the next.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pitched: Vec<u16>,
    pub lanes: Vec<Lane>,
}

impl Pattern {
    pub fn new(id: u16, name: String) -> Self {
        Pattern {
            id,
            name,
            steps: DEFAULT_STEPS,
            colour: None,
            pitched: Vec::new(),
            lanes: Vec::new(),
        }
    }

    /// True if this track is an instrument in this pattern rather than a one-shot.
    pub fn is_pitched(&self, track: u16) -> bool {
        self.pitched.contains(&track)
    }

    /// Make a track an instrument in this pattern, or a one-shot again. Returns what it is
    /// now. Nothing is thrown away either way: the notes are the same notes, and turning it
    /// back on shows them again.
    pub fn set_pitched(&mut self, track: u16, pitched: bool) -> bool {
        let at = self.pitched.iter().position(|&t| t == track);
        match (pitched, at) {
            (true, None) => {
                self.pitched.push(track);
                self.pitched.sort_unstable();
            }
            (false, Some(at)) => {
                self.pitched.remove(at);
            }
            _ => {}
        }
        pitched
    }

    /// The instruments, as one bit per track, which is how the audio thread holds them.
    pub fn pitched_mask(&self) -> u32 {
        self.pitched
            .iter()
            .filter(|&&t| (t as usize) < MAX_TRACKS)
            .fold(0u32, |mask, &t| mask | (1 << t))
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

    /// Add or replace a note, at any pitch and any length. What the piano roll draws with.
    pub fn set_note(&mut self, track: u16, note: Note) -> bool {
        let fits = self.lane_mut(track).set_note(note);
        self.lanes.retain(|l| !l.notes.is_empty());
        fits
    }

    /// Take a note out, wherever it is.
    pub fn clear_note(&mut self, track: u16, step: u32, pitch: u8) -> bool {
        let gone = self.lane_mut(track).clear_note(step, pitch);
        self.lanes.retain(|l| !l.notes.is_empty());
        gone
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
        self.pitched.retain(|&t| t != track);
    }
}

/// One pattern in the song: which pattern, where it starts, and how long it fills.
///
/// A placement starts wherever it was put — the song view snaps to whatever resolution you
/// set, and nothing here cares what that was. `length` starts out as the pattern's own
/// length and can be dragged: longer and the pattern comes round again inside it, shorter and
/// it is cut off. Two placements of the same pattern never overlap, because putting one down
/// takes out whatever it lands on.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "camelCase")]
pub struct Placement {
    pub step: u32,
    pub pattern: u16,
    /// Steps of song it fills. Zero means "as long as the pattern", which is what a project
    /// written before placements could be dragged has, and what [`Project::repair`] fills in.
    #[serde(default)]
    pub length: u32,
}

impl Placement {
    /// One past the last step it covers.
    pub fn end(&self) -> u32 {
        self.step + self.length.max(1)
    }

    pub fn covers(&self, step: u32) -> bool {
        step >= self.step && step < self.end()
    }

    /// True if the two would sound over the top of each other.
    pub fn overlaps(&self, other: &Placement) -> bool {
        self.step < other.end() && other.step < self.end()
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

    /// The placement of a pattern that covers a step, if any. What the song view hit tests
    /// against: the block itself, wherever it was put, rather than a grid worked out from
    /// the pattern's length.
    pub fn placement_at(&self, pattern: u16, step: u32) -> Option<Placement> {
        self.song
            .iter()
            .find(|one| one.pattern == pattern && one.covers(step))
            .copied()
    }

    /// True if this pattern starts exactly here.
    pub fn placed(&self, pattern: u16, step: u32) -> bool {
        self.song
            .iter()
            .any(|one| one.pattern == pattern && one.step == step)
    }

    /// Put a pattern in the song. `length` of zero means "as long as the pattern is".
    ///
    /// Anything of the same pattern it lands on top of makes way for it, the way dropping a
    /// thing on a thing works everywhere else.
    pub fn place(&mut self, pattern: u16, step: u32, length: u32) -> bool {
        let Some(steps) = self.pattern(pattern).map(|p| p.steps.max(1)) else {
            return false;
        };
        let length = if length == 0 { steps } else { length };
        let placed = Placement {
            step,
            pattern,
            length: length.min(MAX_SONG_STEPS as u32),
        };
        if placed.end() > MAX_SONG_STEPS as u32 || self.song.len() >= MAX_PLACEMENTS {
            return false;
        }
        self.song
            .retain(|one| one.pattern != pattern || !one.overlaps(&placed));
        self.song.push(placed);
        self.song.sort_unstable();
        true
    }

    /// Take out the placement of a pattern that starts here.
    pub fn unplace(&mut self, pattern: u16, step: u32) -> bool {
        // Being an instrument used to belong to the track, so it was the same in every
        // pattern. Carry that into every pattern, which is what it sounded like, and clear
        // the old flag so it is never read again.
        let was_pitched: Vec<u16> = self
            .tracks
            .iter()
            .filter(|track| track.pitched)
            .map(|track| track.id)
            .collect();
        if !was_pitched.is_empty() {
            for pattern in &mut self.patterns {
                for &track in &was_pitched {
                    pattern.set_pitched(track, true);
                }
            }
            for track in &mut self.tracks {
                track.pitched = false;
            }
        }
        let before = self.song.len();
        self.song
            .retain(|one| !(one.pattern == pattern && one.step == step));
        before != self.song.len()
    }

    /// Slide a placement along. Keeps how long it is, and takes out anything of the same
    /// pattern it lands on.
    pub fn move_placement(&mut self, pattern: u16, from: u32, to: u32) -> bool {
        let Some(one) = self.placement_at(pattern, from) else {
            return false;
        };
        self.unplace(pattern, one.step);
        if !self.place(pattern, to, one.length) {
            // Would not fit, so put it back where it was rather than losing it.
            self.place(pattern, one.step, one.length);
            return false;
        }
        true
    }

    /// Change how much song a placement fills. One step is the least it can be.
    pub fn resize_placement(&mut self, pattern: u16, step: u32, length: u32) -> bool {
        let Some(one) = self.placement_at(pattern, step) else {
            return false;
        };
        self.unplace(pattern, one.step);
        if !self.place(pattern, one.step, length.max(1)) {
            self.place(pattern, one.step, one.length);
            return false;
        }
        true
    }

    /// Everything that starts inside a bar, gone. What right clicking a bar does: the way
    /// out of a mess without having to pick the pieces off one at a time.
    pub fn clear_bar(&mut self, bar: u32) -> usize {
        let from = bar * STEPS_PER_BAR;
        let to = from + STEPS_PER_BAR;
        // Being an instrument used to belong to the track, so it was the same in every
        // pattern. Carry that into every pattern, which is what it sounded like, and clear
        // the old flag so it is never read again.
        let was_pitched: Vec<u16> = self
            .tracks
            .iter()
            .filter(|track| track.pitched)
            .map(|track| track.id)
            .collect();
        if !was_pitched.is_empty() {
            for pattern in &mut self.patterns {
                for &track in &was_pitched {
                    pattern.set_pitched(track, true);
                }
            }
            for track in &mut self.tracks {
                track.pitched = false;
            }
        }
        let before = self.song.len();
        self.song.retain(|p| p.step < from || p.step >= to);
        before - self.song.len()
    }

    /// Where the song ends, rounded up to a whole bar so it loops somewhere musical.
    pub fn song_steps(&self) -> u32 {
        let end = self.song.iter().map(|one| one.end()).max().unwrap_or(0);
        end.div_ceil(STEPS_PER_BAR) * STEPS_PER_BAR
    }

    pub fn song_bars(&self) -> u32 {
        self.song_steps() / STEPS_PER_BAR
    }

    /// Change a pattern's length. Its places in the song stay where they are and stay as long
    /// as they are: a block is its own length once it is down, and dragging its edge is how
    /// that changes.
    pub fn set_pattern_steps(&mut self, id: u16, steps: u32) -> u32 {
        match self.pattern_mut(id) {
            Some(pattern) => pattern.set_steps(steps),
            None => 0,
        }
    }

    /// Put right anything in a project that this version of the app could not have written.
    ///
    /// Deliberately *not* a tidy-up: it never moves anything that is where somebody put it.
    /// An older version let a pattern's length change without touching its places in the
    /// song, which left blocks the song view could not point at — those are made clickable
    /// by hit testing the block rather than the grid, not by shoving the music about.
    ///
    /// Returns how many placements it had to throw away.
    pub fn repair(&mut self) -> usize {
        let lengths: Vec<(u16, u32)> = self
            .patterns
            .iter()
            .map(|pattern| (pattern.id, pattern.steps.max(1)))
            .collect();
        // Being an instrument used to belong to the track, so it was the same in every
        // pattern. Carry that into every pattern, which is what it sounded like, and clear
        // the old flag so it is never read again.
        let was_pitched: Vec<u16> = self
            .tracks
            .iter()
            .filter(|track| track.pitched)
            .map(|track| track.id)
            .collect();
        if !was_pitched.is_empty() {
            for pattern in &mut self.patterns {
                for &track in &was_pitched {
                    pattern.set_pitched(track, true);
                }
            }
            for track in &mut self.tracks {
                track.pitched = false;
            }
        }
        let before = self.song.len();
        // A placement of a pattern that is not there any more can only confuse things.
        self.song
            .retain(|placement| lengths.iter().any(|(id, _)| *id == placement.pattern));
        // A placement from before blocks had a length of their own is as long as its pattern,
        // which is what it sounded like when it was written.
        for placement in &mut self.song {
            if placement.length == 0 {
                if let Some((_, steps)) = lengths.iter().find(|(id, _)| *id == placement.pattern) {
                    placement.length = *steps;
                }
            }
        }
        self.song.sort_unstable();
        self.song.dedup();
        // Notes that run past the end of a shortened pattern.
        for pattern in &mut self.patterns {
            let steps = pattern.steps;
            for lane in &mut pattern.lanes {
                lane.trim_to(steps);
            }
            pattern.lanes.retain(|lane| !lane.notes.is_empty());
        }
        before - self.song.len()
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
    fn the_piano_roll_writes_into_the_same_lane_as_the_boxes() {
        let mut pattern = Pattern::new(0, "p".into());
        pattern.set_step(0, 0, true);
        // A note somewhere the boxes cannot reach: another pitch, and longer than a step.
        assert!(pattern.set_note(
            0,
            Note {
                step: 4,
                pitch: 67,
                velocity: 90,
                length: 3,
            }
        ));
        assert_eq!(pattern.note_count(), 2);
        // The box is still a box, and the note is not one.
        assert!(pattern.has_step(0, 0));
        assert!(!pattern.has_step(0, 4));

        // Setting one where another already is replaces it rather than doubling up.
        pattern.set_note(
            0,
            Note {
                step: 4,
                pitch: 67,
                velocity: 20,
                length: 1,
            },
        );
        assert_eq!(pattern.note_count(), 2);
        assert_eq!(pattern.lane(0).unwrap().note(4, 67).unwrap().velocity, 20);

        assert!(pattern.clear_note(0, 4, 67));
        assert!(!pattern.clear_note(0, 4, 67));
        assert_eq!(pattern.note_count(), 1);
    }

    #[test]
    fn a_note_that_runs_off_a_shortened_pattern_is_cut_rather_than_lost() {
        let mut pattern = Pattern::new(0, "p".into());
        pattern.set_note(
            0,
            Note {
                step: 4,
                pitch: 60,
                velocity: 100,
                length: 8,
            },
        );
        pattern.set_steps(8);
        let note = pattern.lane(0).unwrap().note(4, 60).unwrap();
        assert_eq!(note.length, 4, "it should reach the end and no further");
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
        assert!(project.place(0, 0, 0));
        assert!(project.place(hats, 0, 0));
        assert!(project.place(snare, 0, 0));
        assert_eq!(project.song.len(), 3);
        assert!(project.placed(hats, 0));
        assert_eq!(project.song_steps(), STEPS_PER_BAR);

        // Taking one out leaves the others where they are.
        assert!(project.unplace(hats, 0));
        assert!(!project.placed(hats, 0));
        assert!(project.placed(snare, 0));
    }

    /// The point of placing by the pattern's own length: a four step pattern takes four steps
    /// of the song, not a whole bar of it.
    #[test]
    fn a_block_is_as_long_as_the_pattern_in_it_until_you_say_otherwise() {
        let mut project = Project::default();
        let short = project.add_pattern().unwrap();
        project.set_pattern_steps(short, 4);

        // One four step pattern at the top: four steps of music, one bar of song.
        assert!(project.place(short, 0, 0));
        assert_eq!(project.placement_at(short, 0).unwrap().length, 4);
        assert!(project.placement_at(short, 3).is_some());
        assert!(
            project.placement_at(short, 4).is_none(),
            "one block is one play through, not a bar of them"
        );
        assert_eq!(
            project.song_steps(),
            STEPS_PER_BAR,
            "the song still loops on the bar"
        );

        // And another one right where the first ended.
        assert!(project.place(short, 4, 0));
        assert_eq!(project.song.len(), 2);

        // Dragging its right edge out makes it repeat rather than making a second block.
        assert!(project.resize_placement(short, 0, 4 * 4));
        assert_eq!(
            project.song.len(),
            1,
            "growing over the next block of the same pattern swallows it"
        );
        assert_eq!(project.placement_at(short, 15).unwrap().step, 0);
    }

    /// The bug behind blocks nobody could click: a block is hit tested where it actually is,
    /// not on a grid worked out from the pattern's length.
    #[test]
    fn a_block_off_the_patterns_grid_can_still_be_pointed_at() {
        let mut project = Project::default();
        project.set_pattern_steps(0, 32);
        // Step 48 is a bar boundary but not a multiple of 32 — where the old grid lost blocks.
        assert!(project.place(0, 48, 0));
        assert!(project.placement_at(0, 48).is_some());
        assert!(project.placement_at(0, 60).is_some());
        assert!(project.unplace(0, 48));
        assert!(project.song.is_empty());
    }

    #[test]
    fn the_song_is_as_long_as_the_last_thing_in_it_rounded_up_to_a_bar() {
        let mut project = Project::default();
        let long = project.add_pattern().unwrap();
        project.set_pattern_steps(long, 32);

        project.place(0, 32, 0); // sixteen steps, at step 32
        assert_eq!(project.song_steps(), 48);
        project.place(long, 64, 0); // thirty two steps, at step 64
        assert_eq!(project.song_steps(), 96);
        assert_eq!(project.song_bars(), 6);
    }

    #[test]
    fn a_pattern_cannot_sound_over_the_top_of_itself() {
        let mut project = Project::default();
        assert!(project.place(0, 48, 0));
        assert!(project.place(0, 48, 0));
        assert_eq!(project.song.len(), 1);
        // Landing halfway across it takes the place of it rather than doubling it up.
        assert!(project.place(0, 56, 0));
        assert_eq!(project.song.len(), 1);
        assert_eq!(project.song[0].step, 56);
    }

    #[test]
    fn a_block_can_be_dragged_along_the_song() {
        let mut project = Project::default();
        project.place(0, 0, 0);
        assert!(project.move_placement(0, 0, 5));
        assert_eq!(project.placement_at(0, 5).unwrap().length, 16);
        assert!(project.placement_at(0, 0).is_none());
        // Grabbing it anywhere along its length moves the whole block.
        assert!(project.move_placement(0, 12, 0));
        assert!(project.placed(0, 0));
        // Dragging past the end of the song we hold leaves it where it was.
        assert!(!project.move_placement(0, 0, MAX_SONG_STEPS as u32 - 4));
        assert!(project.placed(0, 0));
    }

    #[test]
    fn a_block_is_at_least_one_step_long() {
        let mut project = Project::default();
        project.place(0, 0, 0);
        assert!(project.resize_placement(0, 0, 0));
        assert_eq!(project.placement_at(0, 0).unwrap().length, 1);
        assert!(project.placement_at(0, 1).is_none());
    }

    #[test]
    fn clearing_a_bar_takes_out_everything_that_starts_in_it() {
        let mut project = Project::default();
        let short = project.add_pattern().unwrap();
        project.set_pattern_steps(short, 4);
        // Four short placements across the first bar, and one in the second.
        for step in [0, 4, 8, 12, 16] {
            project.place(short, step, 0);
        }
        project.place(0, 0, 0);

        assert_eq!(project.clear_bar(0), 5, "four short ones and the long one");
        assert_eq!(project.song.len(), 1);
        assert!(project.placed(short, 16));
    }

    #[test]
    fn changing_a_length_leaves_the_song_alone() {
        let mut project = Project::default();
        project.set_pattern_steps(0, 4);
        for step in [0, 4, 8, 12] {
            project.place(0, step, 0);
        }
        assert_eq!(project.song.len(), 4);

        // Four steps to sixteen. The blocks are the length they were put down at, so nothing
        // piles up and nothing moves — the music the file describes does not change.
        assert_eq!(project.set_pattern_steps(0, 16), 16);
        assert_eq!(project.song.len(), 4);
        assert!(project.placed(0, 12));
        assert_eq!(project.placement_at(0, 12).unwrap().length, 4);
    }

    #[test]
    fn the_song_will_not_hold_a_pattern_that_does_not_exist() {
        let mut project = Project::default();
        assert!(!project.place(7, 0, 0));
        assert!(project.song.is_empty());
    }

    #[test]
    fn the_song_has_an_end_to_it() {
        let mut project = Project::default();
        // A step way past the longest song we hold is refused rather than wrapped.
        assert!(!project.place(0, 100_000, 0));
        // And so is a block that would run off the end.
        assert!(!project.place(0, MAX_SONG_STEPS as u32 - 4, 16));
        assert!(project.song.is_empty());
    }

    #[test]
    fn deleting_a_pattern_takes_it_out_of_the_song() {
        let mut project = Project::default();
        let second = project.add_pattern().unwrap();
        project.place(0, 0, 0);
        project.place(second, 0, 0);
        project.place(second, 16, 0);

        assert!(project.remove_pattern(second));
        assert_eq!(project.song.len(), 1);
        assert!(project.placed(0, 0));

        // The last pattern stays put, whatever anyone asks.
        assert!(!project.remove_pattern(0));
        assert_eq!(project.patterns.len(), 1);
    }

    /// Being an instrument is the pattern's business, so two patterns can disagree about the
    /// same track.
    #[test]
    fn a_track_can_be_an_instrument_in_one_pattern_and_a_drum_in_another() {
        let mut project = kit();
        let second = project.add_pattern().unwrap();
        assert!(project.pattern_mut(second).unwrap().set_pitched(0, true));
        assert!(project.pattern(second).unwrap().is_pitched(0));
        assert!(!project.pattern(0).unwrap().is_pitched(0));
        assert_eq!(project.pattern(second).unwrap().pitched_mask(), 1);
        assert_eq!(project.pattern(0).unwrap().pitched_mask(), 0);

        // A copy of a pattern carries it, because it is part of the part.
        let copy = project.duplicate_pattern(second).unwrap();
        assert!(project.pattern(copy).unwrap().is_pitched(0));

        // And deleting the track takes it out of every pattern.
        project.remove_track(0);
        assert!(!project.pattern(second).unwrap().is_pitched(0));
    }

    /// It used to belong to the track, which meant every pattern. An old project has to sound
    /// the way it did, so opening one marks every pattern for a track that was pitched.
    #[test]
    fn an_old_project_keeps_its_instruments() {
        let mut project = kit();
        project.add_pattern().unwrap();
        project.track_mut(1).unwrap().pitched = true;

        project.repair();
        for pattern in &project.patterns {
            assert!(pattern.is_pitched(1), "pattern {} lost it", pattern.id);
            assert!(!pattern.is_pitched(0));
        }
        assert!(
            !project.track(1).unwrap().pitched,
            "the old flag is still set, so it would be read again"
        );
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
        project.place(0, 0, 0);
        project.place(second, 0, 0);
        project.place(second, 32, 0);

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
        assert_eq!(
            back.placement_at(second, 40).unwrap().length,
            32,
            "a thirty two step pattern, and the block knows it is that long"
        );
    }
}
