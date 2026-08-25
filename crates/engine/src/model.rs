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
    DEFAULT_PITCH, DEFAULT_STEPS, DEFAULT_TRACK_GAIN, MAX_NOTES_PER_TRACK, MAX_PATTERNS,
    MAX_PLACEMENTS, MAX_SONG_STEPS, MAX_STEPS, MAX_TRACKS, STEPS_PER_BAR,
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

/// How a track's sound is played: the shape it is given, where it sits, and how much of the
/// file is used.
///
/// The track's, not the pattern's, and that is the whole distinction. How loud a part is and
/// whether you hear it at all is writing the part, so it belongs to the pattern; what the
/// sound *is* — how sharply it starts, how long it hangs on, where it sits between the
/// speakers, how it is tuned — is the same wherever it is played, so it belongs here. Change
/// it and every pattern using that sound changes with it, which is what you want when the
/// snare's tail is too long.
///
/// Times are in seconds and everything else is a plain fraction, because those are the units
/// the controls are labelled in. The engine turns them into per-frame increments once, when
/// they arrive, rather than on every note.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Voicing {
    /// Seconds from silence up to full. Never quite zero: a sample that does not start at
    /// zero needs a moment or it clicks.
    #[serde(default = "default_attack")]
    pub attack: f32,
    /// Seconds from full down to the sustain level.
    #[serde(default)]
    pub decay: f32,
    /// The level it settles at while the note is held, as a fraction of full.
    #[serde(default = "default_sustain")]
    pub sustain: f32,
    /// Seconds from wherever it is down to silence, once the note ends.
    #[serde(default = "default_release")]
    pub release: f32,
    /// Where it sits between the speakers: -1 hard left, 0 middle, 1 hard right.
    #[serde(default)]
    pub pan: f32,
    /// Semitones up or down, fractions allowed. On top of whatever pitch the note is.
    #[serde(default)]
    pub tune: f32,
    /// A trim on the sound itself, on top of the pattern's fader. 1.0 leaves it alone.
    #[serde(default = "default_level")]
    pub level: f32,
    /// How far into the file a note starts, as a fraction of its length. For trimming the
    /// silence off the front of a sample somebody else recorded.
    #[serde(default)]
    pub start: f32,
    /// And where it stops. Always past `start`; the engine holds it there.
    #[serde(default = "default_end")]
    pub end: f32,
}

/// A moment, not nothing: enough to swallow the step in a sample that does not start at zero
/// without softening a drum hit. About two milliseconds.
pub const DEFAULT_ATTACK: f32 = 0.002;
/// And about three on the way out, which is the shortest fade that does not click.
pub const DEFAULT_RELEASE: f32 = 0.003;

fn default_attack() -> f32 {
    DEFAULT_ATTACK
}

fn default_sustain() -> f32 {
    1.0
}

fn default_release() -> f32 {
    DEFAULT_RELEASE
}

fn default_level() -> f32 {
    1.0
}

fn default_end() -> f32 {
    1.0
}

impl Default for Voicing {
    /// The sound as it came off the disk: a fade at each end short enough not to be heard,
    /// nothing else touched.
    fn default() -> Self {
        Voicing {
            attack: DEFAULT_ATTACK,
            decay: 0.0,
            sustain: 1.0,
            release: DEFAULT_RELEASE,
            pan: 0.0,
            tune: 0.0,
            level: 1.0,
            start: 0.0,
            end: 1.0,
        }
    }
}

impl Voicing {
    /// Put every control back inside the range its editor offers. Everything from outside
    /// this crate goes through here, so the engine can trust what it is given.
    pub fn settled(self) -> Self {
        let start = self.start.clamp(0.0, 0.99);
        Voicing {
            attack: clamp_time(self.attack),
            decay: clamp_time(self.decay),
            sustain: self.sustain.clamp(0.0, 1.0),
            release: clamp_time(self.release),
            pan: self.pan.clamp(-1.0, 1.0),
            tune: self.tune.clamp(-24.0, 24.0),
            level: self.level.clamp(0.0, 2.0),
            start,
            // A note has to be allowed to make some sound, so the end never catches the start.
            end: self.end.clamp(start + 0.01, 1.0),
        }
    }

    /// True when nothing has been changed, which is when there is nothing worth writing down.
    pub fn untouched(&self) -> bool {
        *self == Voicing::default()
    }
}

/// The longest any one stage of the envelope can be. Ten seconds is a pad's release; past
/// that it is a loop, not a note.
const MAX_ENVELOPE_SECS: f32 = 10.0;

fn clamp_time(secs: f32) -> f32 {
    if secs.is_finite() {
        secs.clamp(0.0, MAX_ENVELOPE_SECS)
    } else {
        0.0
    }
}

/// A CLAP instrument on a track, as the project remembers it.
///
/// The path and the id are how it is found again. The name is kept as well so a project that
/// has been carried to a machine without that plugin can say *which* plugin is missing rather
/// than only that one is.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PluginRef {
    /// The `.clap` file or bundle, as an absolute path. Not copied into the project folder
    /// the way a sample is: a plugin is installed software, often hundreds of megabytes, and
    /// the licence to copy it is not ours to assume.
    pub path: String,
    /// Its id inside that file, which never changes for the life of the plugin.
    pub id: String,
    pub name: String,
    /// Where its own settings live inside the project folder, e.g. `plugins/0.clapstate`.
    /// The plugin decides what is in there; we only carry it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
}

/// One instrument: a sound and how loud it is. Belongs to the project, not to a pattern.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Track {
    /// Unique among live tracks, and also the track's slot in the audio engine.
    pub id: u16,
    pub name: String,
    pub sample: Option<SampleRef>,
    /// A CLAP instrument instead of a sample. A track has one or the other: adding a plugin
    /// puts the sample down, and adding a sample puts the plugin down.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin: Option<PluginRef>,
    /// How the sound is played: its envelope, where it sits, how it is tuned, and how much
    /// of the file a note uses. The track's own, so every pattern hears the same sound.
    ///
    /// A plugin has its own envelope, its own tuning and its own idea of where it sits, so
    /// the only part of this that means anything on a plugin track is how loud it is.
    #[serde(default)]
    pub voicing: Voicing,
    /// How loud, whether it is heard, and whether it is an instrument — all left over from
    /// when they belonged to the track rather than to each pattern.
    ///
    /// Read once on the way in, by [`Project::repair`], which writes them into every pattern
    /// and then puts them back to their defaults, so a project written before the mixer moved
    /// sounds the way it did. Never meaningful in a file this version wrote.
    #[serde(default = "default_gain")]
    pub gain: f32,
    #[serde(default)]
    pub muted: bool,
    #[serde(default)]
    pub soloed: bool,
    #[serde(default)]
    pub pitched: bool,
}

impl Track {
    pub fn new(id: u16, name: String, sample: Option<SampleRef>) -> Self {
        Track {
            id,
            name,
            sample,
            plugin: None,
            voicing: Voicing::default(),
            gain: DEFAULT_TRACK_GAIN,
            muted: false,
            soloed: false,
            pitched: false,
        }
    }

    /// True when nothing in the old per-track mixer was ever moved, so there is nothing to
    /// carry into the patterns.
    fn mixer_untouched(&self) -> bool {
        self.gain == DEFAULT_TRACK_GAIN && !self.muted && !self.soloed && !self.pitched
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

/// How one track sits in one pattern: how loud, whether it is heard, and whether it is
/// played as an instrument or as a one-shot.
///
/// Per pattern, because all four are decisions about the part rather than about the sound.
/// The same kick can be loud in the chorus and half its level in the verse, and the same bass
/// can hold a rhythm down as a row of boxes in one pattern and play a melody in the next.
///
/// Only tracks that differ from the default get one of these, so a project full of patterns
/// nobody has touched the mixer in costs nothing.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TrackMix {
    pub track: u16,
    /// Linear, 0.0 to 1.5. 1.0 is unity.
    #[serde(default = "default_gain")]
    pub gain: f32,
    #[serde(default)]
    pub muted: bool,
    #[serde(default)]
    pub soloed: bool,
    /// A sampler instrument rather than a one-shot: pitched across the keyboard, each note
    /// stopping when it ends, and edited as a piano roll instead of a row of boxes.
    #[serde(default)]
    pub pitched: bool,
}

fn default_gain() -> f32 {
    DEFAULT_TRACK_GAIN
}

/// A pattern nobody has silenced, which is nearly all of them: not worth writing down.
fn not_muted(muted: &bool) -> bool {
    !*muted
}

impl TrackMix {
    pub fn new(track: u16) -> Self {
        TrackMix {
            track,
            gain: DEFAULT_TRACK_GAIN,
            muted: false,
            soloed: false,
            pitched: false,
        }
    }

    /// True when nothing has been changed from how a new pattern starts, which is when the
    /// record is not worth writing down.
    pub fn untouched(&self) -> bool {
        *self == TrackMix::new(self.track)
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
    /// Silent, wherever it plays. The speaker on its row in the panel.
    ///
    /// A different switch from the mute on a track's row, which is one track inside one
    /// pattern. This one is the pattern itself: turn the hats pattern off and every block of
    /// it in the song goes quiet at once, which is how you listen to a song without a part
    /// without taking the part out.
    #[serde(default, skip_serializing_if = "not_muted")]
    pub muted: bool,
    /// How each track sits in this pattern. Only the ones that differ from the default are
    /// in here; [`Pattern::mix_of`] answers for the rest.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mix: Vec<TrackMix>,
    /// Left over from the version that kept only the instruments, as a list of track ids.
    /// Folded into `mix` by [`Project::repair`] and then cleared. Never written.
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
            muted: false,
            mix: Vec::new(),
            pitched: Vec::new(),
            lanes: Vec::new(),
        }
    }

    /// How a track sits in this pattern. Every track has an answer, whether or not anybody
    /// has touched it.
    pub fn mix_of(&self, track: u16) -> TrackMix {
        self.mix
            .iter()
            .find(|one| one.track == track)
            .copied()
            .unwrap_or_else(|| TrackMix::new(track))
    }

    /// The same, to write into. Makes the record if there is not one yet.
    fn mix_mut(&mut self, track: u16) -> &mut TrackMix {
        if let Some(at) = self.mix.iter().position(|one| one.track == track) {
            return &mut self.mix[at];
        }
        self.mix.push(TrackMix::new(track));
        self.mix.sort_unstable_by_key(|one| one.track);
        let at = self.mix.iter().position(|one| one.track == track).unwrap();
        &mut self.mix[at]
    }

    /// Change one thing about how a track sits in this pattern, and throw the record away
    /// again if it is back to how a new pattern starts.
    pub fn set_mix(&mut self, track: u16, change: impl FnOnce(&mut TrackMix)) {
        change(self.mix_mut(track));
        self.mix.retain(|one| !one.untouched());
    }

    pub fn set_gain(&mut self, track: u16, gain: f32) -> f32 {
        let settled = gain.clamp(0.0, 1.5);
        self.set_mix(track, |mix| mix.gain = settled);
        settled
    }

    pub fn set_muted(&mut self, track: u16, muted: bool) {
        self.set_mix(track, |mix| mix.muted = muted);
    }

    pub fn set_soloed(&mut self, track: u16, soloed: bool) {
        self.set_mix(track, |mix| mix.soloed = soloed);
    }

    /// True if this track is an instrument in this pattern rather than a one-shot.
    pub fn is_pitched(&self, track: u16) -> bool {
        self.mix_of(track).pitched
    }

    /// Make a track an instrument in this pattern, or a one-shot again. Nothing is thrown
    /// away either way: the notes are the same notes, and turning it back on shows them.
    pub fn set_pitched(&mut self, track: u16, pitched: bool) -> bool {
        self.set_mix(track, |mix| mix.pitched = pitched);
        pitched
    }

    /// The instruments, as one bit per track, which is how the audio thread holds them.
    pub fn pitched_mask(&self) -> u32 {
        self.mix
            .iter()
            .filter(|one| one.pitched && (one.track as usize) < MAX_TRACKS)
            .fold(0u32, |mask, one| mask | (1 << one.track))
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
        self.mix.retain(|one| one.track != track);
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

    /// Delete a track, and with it every note anyone had drawn for it.
    pub fn remove_track(&mut self, id: u16) -> Option<Track> {
        let at = self.tracks.iter().position(|t| t.id == id)?;
        for pattern in &mut self.patterns {
            pattern.forget_track(id);
        }
        Some(self.tracks.remove(at))
    }

    /// Move a track to a different place in the list. `to` is where it ends up once it has
    /// been taken out, so dragging the top one to the bottom is `to == len - 1`.
    ///
    /// Nothing but the order changes. A track's id is its slot in the engine and the key its
    /// notes are kept under in every pattern, so the ids stay where they are and the audio
    /// thread never has to hear about this at all: it is which row a sound is drawn on.
    pub fn move_track(&mut self, id: u16, to: usize) -> bool {
        let Some(at) = self.tracks.iter().position(|t| t.id == id) else {
            return false;
        };
        let to = to.min(self.tracks.len().saturating_sub(1));
        if to == at {
            return false;
        }
        let one = self.tracks.remove(at);
        self.tracks.insert(to, one);
        true
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

    /// Move a pattern to a different place in the list, which is also which lane it is in
    /// the song. `to` is where it ends up once it has been taken out.
    ///
    /// Ids do not move. The song says which pattern plays where by id, and a pattern's id is
    /// its slot in the engine, so this is only the order they are shown in.
    ///
    /// A pattern nobody has given a colour is drawn in the colour of its place in the list,
    /// so moving one would otherwise make two patterns swap colours behind your back. Every
    /// pattern keeps the colour it has right now instead, written down as its own.
    pub fn move_pattern(&mut self, id: u16, to: usize) -> bool {
        let Some(at) = self.patterns.iter().position(|p| p.id == id) else {
            return false;
        };
        let to = to.min(self.patterns.len().saturating_sub(1));
        if to == at {
            return false;
        }
        for (place, pattern) in self.patterns.iter_mut().enumerate() {
            if pattern.colour.is_none() {
                pattern.colour = Some(place as u8);
            }
        }
        let one = self.patterns.remove(at);
        self.patterns.insert(to, one);
        true
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
        // How loud a track is, whether it is heard and whether it is an instrument all used
        // to belong to the track, so they were the same in every pattern. Carry them into
        // every pattern — which is what the project sounded like — and put the old fields
        // back to their defaults so they are never read again.
        let carried: Vec<TrackMix> = self
            .tracks
            .iter()
            .filter(|track| !track.mixer_untouched())
            .map(|track| TrackMix {
                track: track.id,
                gain: track.gain,
                muted: track.muted,
                soloed: track.soloed,
                pitched: track.pitched,
            })
            .collect();
        for pattern in &mut self.patterns {
            // A list of instruments, from the version between the two.
            let listed = std::mem::take(&mut pattern.pitched);
            for track in listed {
                pattern.set_pitched(track, true);
            }
            for was in &carried {
                // Whatever the pattern says for itself wins: it was written later.
                if pattern.mix.iter().any(|one| one.track == was.track) {
                    continue;
                }
                pattern.set_mix(was.track, |mix| *mix = *was);
            }
            // A record for a track that is gone is only in the way.
            let live: Vec<u16> = self.tracks.iter().map(|t| t.id).collect();
            pattern.mix.retain(|one| live.contains(&one.track));
        }
        for track in &mut self.tracks {
            track.gain = DEFAULT_TRACK_GAIN;
            track.muted = false;
            track.soloed = false;
            track.pitched = false;
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
        // How loud a track is, whether it is heard and whether it is an instrument all used
        // to belong to the track, so they were the same in every pattern. Carry them into
        // every pattern — which is what the project sounded like — and put the old fields
        // back to their defaults so they are never read again.
        let carried: Vec<TrackMix> = self
            .tracks
            .iter()
            .filter(|track| !track.mixer_untouched())
            .map(|track| TrackMix {
                track: track.id,
                gain: track.gain,
                muted: track.muted,
                soloed: track.soloed,
                pitched: track.pitched,
            })
            .collect();
        for pattern in &mut self.patterns {
            // A list of instruments, from the version between the two.
            let listed = std::mem::take(&mut pattern.pitched);
            for track in listed {
                pattern.set_pitched(track, true);
            }
            for was in &carried {
                // Whatever the pattern says for itself wins: it was written later.
                if pattern.mix.iter().any(|one| one.track == was.track) {
                    continue;
                }
                pattern.set_mix(was.track, |mix| *mix = *was);
            }
            // A record for a track that is gone is only in the way.
            let live: Vec<u16> = self.tracks.iter().map(|t| t.id).collect();
            pattern.mix.retain(|one| live.contains(&one.track));
        }
        for track in &mut self.tracks {
            track.gain = DEFAULT_TRACK_GAIN;
            track.muted = false;
            track.soloed = false;
            track.pitched = false;
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
        // How loud a track is, whether it is heard and whether it is an instrument all used
        // to belong to the track, so they were the same in every pattern. Carry them into
        // every pattern — which is what the project sounded like — and put the old fields
        // back to their defaults so they are never read again.
        let carried: Vec<TrackMix> = self
            .tracks
            .iter()
            .filter(|track| !track.mixer_untouched())
            .map(|track| TrackMix {
                track: track.id,
                gain: track.gain,
                muted: track.muted,
                soloed: track.soloed,
                pitched: track.pitched,
            })
            .collect();
        for pattern in &mut self.patterns {
            // A list of instruments, from the version between the two.
            let listed = std::mem::take(&mut pattern.pitched);
            for track in listed {
                pattern.set_pitched(track, true);
            }
            for was in &carried {
                // Whatever the pattern says for itself wins: it was written later.
                if pattern.mix.iter().any(|one| one.track == was.track) {
                    continue;
                }
                pattern.set_mix(was.track, |mix| *mix = *was);
            }
            // A record for a track that is gone is only in the way.
            let live: Vec<u16> = self.tracks.iter().map(|t| t.id).collect();
            pattern.mix.retain(|one| live.contains(&one.track));
        }
        for track in &mut self.tracks {
            track.gain = DEFAULT_TRACK_GAIN;
            track.muted = false;
            track.soloed = false;
            track.pitched = false;
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

    /// The pattern's own mute is the pattern's, so it is written down with it — but only
    /// when it is on, because nearly every pattern is one nobody has silenced.
    #[test]
    fn a_silenced_pattern_is_still_silenced_when_it_is_opened_again() {
        let mut project = kit();
        // A pattern nobody has silenced does not carry the word at all.
        assert!(!serde_json::to_string(&project.patterns[0])
            .unwrap()
            .contains("muted"));
        project.patterns[0].muted = true;
        let text = serde_json::to_string(&project).unwrap();
        let back: Project = serde_json::from_str(&text).unwrap();
        assert!(back.patterns[0].muted);
    }

    /// Reordering is only the order. A pattern's id is what the song and the engine know it
    /// by, so dragging one up the list must not move a single block.
    #[test]
    fn moving_a_pattern_up_the_list_leaves_the_song_alone() {
        let mut project = kit();
        project.add_pattern();
        project.add_pattern();
        let ids: Vec<u16> = project.patterns.iter().map(|p| p.id).collect();
        project.place(ids[2], 32, 16);
        assert!(project.move_pattern(ids[2], 0));
        assert_eq!(
            project.patterns.iter().map(|p| p.id).collect::<Vec<_>>(),
            vec![ids[2], ids[0], ids[1]]
        );
        assert_eq!(project.song.len(), 1);
        assert_eq!(project.song[0].pattern, ids[2]);
        assert_eq!(project.song[0].step, 32);
        // And nothing changes colour under you: the one that moved keeps the colour it was
        // being drawn in, which was its old place in the list.
        assert_eq!(project.pattern(ids[2]).unwrap().colour, Some(2));
        assert_eq!(project.pattern(ids[0]).unwrap().colour, Some(0));
    }

    #[test]
    fn moving_a_track_keeps_its_notes_and_its_slot() {
        let mut project = kit();
        project.patterns[0].set_step(1, 4, true);
        assert!(project.move_track(1, 0));
        assert_eq!(
            project.tracks.iter().map(|t| t.id).collect::<Vec<_>>(),
            vec![1, 0]
        );
        assert!(project.patterns[0].has_step(1, 4));
        // Nowhere to move to, and a track that is not there, are both nothing happening.
        assert!(!project.move_track(1, 0));
        assert!(!project.move_track(9, 0));
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

    /// The mixer used to belong to the track, which meant every pattern. An old project has
    /// to sound the way it did, so opening one writes what the track said into every pattern.
    #[test]
    fn an_old_project_keeps_its_mixer() {
        let mut project = kit();
        project.add_pattern().unwrap();
        {
            let track = project.track_mut(1).unwrap();
            track.pitched = true;
            track.gain = 0.25;
            track.muted = true;
        }

        project.repair();
        for pattern in &project.patterns {
            let mix = pattern.mix_of(1);
            assert!(mix.pitched, "pattern {} lost the instrument", pattern.id);
            assert_eq!(mix.gain, 0.25, "pattern {} lost the level", pattern.id);
            assert!(mix.muted, "pattern {} lost the mute", pattern.id);
            // And a track nobody touched is left out of it entirely.
            assert_eq!(pattern.mix_of(0), TrackMix::new(0));
            assert!(pattern.mix.iter().all(|one| one.track != 0));
        }
        let track = project.track(1).unwrap();
        assert!(
            track.mixer_untouched(),
            "the old fields are still set, so they would be read again"
        );
    }

    /// Something the pattern says for itself wins: it was written by a version that already
    /// knew the mixer belonged to the pattern.
    #[test]
    fn what_a_pattern_says_beats_what_the_track_used_to() {
        let mut project = kit();
        project.pattern_mut(0).unwrap().set_gain(1, 1.2);
        project.track_mut(1).unwrap().gain = 0.25;

        project.repair();
        assert_eq!(project.pattern(0).unwrap().mix_of(1).gain, 1.2);
    }

    /// Turning something back to where it started leaves nothing behind.
    #[test]
    fn a_mixer_nobody_has_touched_is_not_written_down() {
        let mut project = kit();
        let pattern = project.pattern_mut(0).unwrap();
        pattern.set_gain(0, 0.3);
        assert_eq!(pattern.mix.len(), 1);
        pattern.set_gain(0, DEFAULT_TRACK_GAIN);
        assert!(pattern.mix.is_empty(), "an untouched record was kept");

        // Unless something else about it is still changed.
        pattern.set_muted(0, true);
        pattern.set_gain(0, DEFAULT_TRACK_GAIN);
        assert_eq!(pattern.mix.len(), 1);
        assert!(pattern.mix_of(0).muted);
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

    #[test]
    fn a_shaped_sound_survives_being_written_out() {
        let mut project = Project::default();
        project.tracks.push(Track::new(0, "pad".into(), None));
        project.tracks[0].voicing = Voicing {
            attack: 0.4,
            decay: 0.2,
            sustain: 0.5,
            release: 1.5,
            pan: -0.6,
            tune: 7.0,
            level: 0.75,
            start: 0.1,
            end: 0.9,
        };

        let json = serde_json::to_string(&project).unwrap();
        let back: Project = serde_json::from_str(&json).unwrap();
        assert_eq!(back.tracks[0].voicing, project.tracks[0].voicing);
    }

    #[test]
    fn a_project_written_before_there_were_sounds_to_shape_still_opens() {
        // Version 2 as it was: a track with no voicing at all. It has to come back as one
        // nobody has shaped, not as silence or a panic.
        let json = r#"{
            "version": 2,
            "bpm": 120.0,
            "masterGain": 0.9,
            "tracks": [{ "id": 0, "name": "kick", "sample": null }],
            "patterns": [],
            "song": []
        }"#;
        let back: Project = serde_json::from_str(json).unwrap();
        assert_eq!(back.tracks[0].voicing, Voicing::default());
        assert!(back.tracks[0].voicing.untouched());
    }

    #[test]
    fn a_voicing_from_outside_is_pushed_back_inside_its_range() {
        let mad = Voicing {
            attack: -1.0,
            decay: f32::INFINITY,
            sustain: 4.0,
            release: 900.0,
            pan: -8.0,
            tune: 100.0,
            level: -3.0,
            // The wrong way round, which is the one that would otherwise silence the track.
            start: 0.8,
            end: 0.2,
        }
        .settled();
        assert_eq!(mad.attack, 0.0);
        assert_eq!(mad.decay, 0.0, "an infinite decay is not a decay");
        assert_eq!(mad.sustain, 1.0);
        assert_eq!(mad.release, 10.0);
        assert_eq!(mad.pan, -1.0);
        assert_eq!(mad.tune, 24.0);
        assert_eq!(mad.level, 0.0);
        assert!(
            mad.end > mad.start,
            "a note has to be allowed to make some sound: {} to {}",
            mad.start,
            mad.end
        );
    }
}
