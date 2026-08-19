//! The project. Plain data, owned by the app thread, serialised straight to `project.json`.
//!
//! Steps are stored as notes, not booleans, from day one. A step box is a note at
//! [`DEFAULT_PITCH`] one step long, so the piano roll in stage 4 is a different editor over
//! the same data rather than a file format migration.

use serde::{Deserialize, Serialize};

use crate::{DEFAULT_PITCH, MAX_NOTES_PER_TRACK, MAX_TRACKS};

/// Bumped whenever the on-disk shape changes.
pub const PROJECT_VERSION: u32 = 1;

/// A note in a pattern. `step` and `length` are in steps, `pitch` is MIDI.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
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
pub struct SampleRef {
    /// Absolute for now. Stage 2 makes this relative to the project folder.
    pub path: String,
    pub name: String,
}

/// One row of the grid.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Track {
    /// Unique among live tracks, and also the track's slot in the audio engine.
    pub id: u16,
    pub name: String,
    pub sample: Option<SampleRef>,
    /// Linear, 0.0 to 1.5. 1.0 is unity.
    pub gain: f32,
    pub muted: bool,
    pub soloed: bool,
    pub notes: Vec<Note>,
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
    /// was asked for if the track is full.
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

/// A pattern: a fixed number of steps, and the tracks that play in it.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Pattern {
    pub name: String,
    pub steps: u32,
    pub tracks: Vec<Track>,
}

impl Default for Pattern {
    fn default() -> Self {
        Pattern {
            name: "Pattern 1".into(),
            steps: 16,
            tracks: Vec::new(),
        }
    }
}

/// Everything the app knows about the song. Stage 2 turns `pattern` into a list.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Project {
    pub version: u32,
    pub bpm: f32,
    /// Linear master gain before the soft clipper.
    pub master_gain: f32,
    pub pattern: Pattern,
}

impl Default for Project {
    fn default() -> Self {
        Project {
            version: PROJECT_VERSION,
            bpm: 120.0,
            master_gain: 0.9,
            pattern: Pattern::default(),
        }
    }
}

impl Project {
    pub fn track(&self, id: u16) -> Option<&Track> {
        self.pattern.tracks.iter().find(|t| t.id == id)
    }

    pub fn track_mut(&mut self, id: u16) -> Option<&mut Track> {
        self.pattern.tracks.iter_mut().find(|t| t.id == id)
    }

    /// Lowest free engine slot, or `None` when every slot is taken.
    pub fn free_track_id(&self) -> Option<u16> {
        (0..MAX_TRACKS as u16).find(|id| self.track(*id).is_none())
    }

    /// True while any track is soloed, which is when mutes stop mattering.
    pub fn any_soloed(&self) -> bool {
        self.pattern.tracks.iter().any(|t| t.soloed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_step_box_is_a_note() {
        let mut track = Track::new(0, "kick".into(), None);
        assert!(!track.has_step(4));
        assert!(track.set_step(4, true));
        assert!(track.has_step(4));
        // Stored as a note at the sampler's unity pitch, which is what makes the piano
        // roll in stage 4 a different editor rather than a file format migration.
        assert_eq!(track.notes[0], Note::step_note(4));
        assert_eq!(track.notes[0].pitch, DEFAULT_PITCH);
        assert!(!track.set_step(4, false));
        assert!(track.notes.is_empty());
    }

    #[test]
    fn ticking_a_ticked_box_changes_nothing() {
        let mut track = Track::new(0, "kick".into(), None);
        track.set_step(2, true);
        track.set_step(2, true);
        assert_eq!(track.notes.len(), 1);
    }

    #[test]
    fn a_track_will_not_grow_past_what_the_engine_holds() {
        let mut track = Track::new(0, "kick".into(), None);
        for step in 0..(MAX_NOTES_PER_TRACK as u32 + 20) {
            track.set_step(step, true);
        }
        assert_eq!(track.notes.len(), MAX_NOTES_PER_TRACK);
    }

    #[test]
    fn shortening_a_pattern_drops_the_notes_that_fall_off() {
        let mut track = Track::new(0, "kick".into(), None);
        for step in 0..16 {
            track.set_step(step, true);
        }
        track.trim_to(8);
        assert_eq!(track.notes.len(), 8);
        assert!(track.notes.iter().all(|n| n.step < 8));
    }

    #[test]
    fn track_ids_are_the_lowest_free_engine_slot() {
        let mut project = Project::default();
        assert_eq!(project.free_track_id(), Some(0));
        project.pattern.tracks.push(Track::new(0, "a".into(), None));
        project.pattern.tracks.push(Track::new(1, "b".into(), None));
        assert_eq!(project.free_track_id(), Some(2));

        // Deleting the middle one frees its slot for the next track, so the engine never
        // has to shuffle its state when a row is removed.
        project.pattern.tracks.retain(|t| t.id != 0);
        assert_eq!(project.free_track_id(), Some(0));
    }

    #[test]
    fn slots_run_out_rather_than_overflowing() {
        let mut project = Project::default();
        for id in 0..MAX_TRACKS as u16 {
            project
                .pattern
                .tracks
                .push(Track::new(id, "x".into(), None));
        }
        assert_eq!(project.free_track_id(), None);
    }

    #[test]
    fn survives_a_round_trip_through_json() {
        let mut project = Project {
            bpm: 138.0,
            ..Default::default()
        };
        let mut track = Track::new(
            0,
            "kick".into(),
            Some(SampleRef {
                path: "/samples/kick.wav".into(),
                name: "kick".into(),
            }),
        );
        track.set_step(0, true);
        track.set_step(8, true);
        project.pattern.tracks.push(track);

        let json = serde_json::to_string(&project).unwrap();
        let back: Project = serde_json::from_str(&json).unwrap();
        assert_eq!(back.bpm, 138.0);
        assert_eq!(back.version, PROJECT_VERSION);
        assert_eq!(back.pattern.tracks.len(), 1);
        assert!(back.pattern.tracks[0].has_step(8));
        assert_eq!(back.pattern.tracks[0].sample.as_ref().unwrap().name, "kick");
    }
}
