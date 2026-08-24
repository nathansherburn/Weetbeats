//! What the app thread says to the audio thread, and what comes back.
//!
//! One `rtrb` ring buffer each way. Commands are small and `Copy`-ish so a push is a
//! memcpy of a couple of words. Nothing here allocates on either side.

use std::sync::Arc;

use crate::model::Voicing;
use crate::plugins::Slot;
use crate::sample::Sample;

/// A note as the engine holds it: steps and MIDI pitch, sized down so [`Command`] stays small.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct EngineNote {
    pub step: u16,
    pub pitch: u8,
    pub velocity: u8,
    /// In steps. Unused by one-shot samples, waiting for the sampler in stage 3.
    pub length: u16,
}

/// App thread to audio thread. Never blocks either side.
///
/// Patterns and tracks are addressed by their id, which is also their slot, so nothing
/// here ever has to say "the pattern that used to be third".
#[derive(Debug)]
pub enum Command {
    /// Start or stop the transport. Stopping leaves ringing voices to finish.
    SetPlaying(bool),
    /// Jump to the top of what is playing.
    Rewind,
    SetBpm(f32),
    SetMasterGain(f32),
    /// Claim a slot. A track with no sample is silent but keeps its notes. How loud it is
    /// belongs to each pattern, so there is nothing to say about level here.
    AddTrack {
        track: u16,
    },
    /// Free a slot, release anything it was holding, and forget its notes in every pattern.
    RemoveTrack {
        track: u16,
    },
    SetTrackSample {
        track: u16,
        sample: Option<Arc<Sample>>,
    },
    /// Put a CLAP instrument on a track, or take it off. Everything the audio thread needs is
    /// inside the box and was allocated on the app thread; a slot coming off goes back there
    /// to be dropped, because freeing it here would be a stall.
    ///
    /// A track plays a plugin *or* a sample. Sending one clears the other.
    SetTrackPlugin {
        track: u16,
        slot: Option<Box<Slot>>,
    },
    /// Move one of a plugin's parameters. Goes in as an event on the next block, which is the
    /// only way CLAP has of moving one while it plays.
    SetPluginParam {
        track: u16,
        param: u32,
        value: f64,
    },
    /// How the track's sound is played: its envelope, where it sits between the speakers, how
    /// it is tuned, its level trim and how much of the file a note reads.
    ///
    /// The track's, not a pattern's, so this arrives once however many patterns use it. Only
    /// the next note hears it: a voice keeps the shape it started with, so dragging the
    /// attack about while it plays does not warp what is already sounding.
    SetTrackVoicing {
        track: u16,
        voicing: Voicing,
    },
    /// How loud a track is in one pattern. Slides to its new value rather than jumping, so
    /// a fader moved while it plays takes what is already sounding with it.
    SetPatternGain {
        pattern: u16,
        track: u16,
        gain: f32,
    },
    /// Silent in this pattern. Fades out rather than cutting, for the same reason. Beats
    /// solo: a track that is both is silent, so mute always means the one thing.
    SetPatternMuted {
        pattern: u16,
        track: u16,
        muted: bool,
    },
    /// The whole pattern silent, wherever it plays. The speaker on its row in the panel.
    ///
    /// A different switch from `SetPatternMuted`, which is one track inside one pattern. This
    /// one is the pattern: every track in it goes quiet, in every block of it in the song, and
    /// what it is already holding goes down with it.
    MutePattern {
        pattern: u16,
        muted: bool,
    },
    /// Anything soloed in a pattern means only the soloed tracks are heard in it — of the
    /// ones that are not muted, because mute wins.
    SetPatternSoloed {
        pattern: u16,
        track: u16,
        soloed: bool,
    },
    /// True for a sampler instrument in this pattern, false for a one-shot. An instrument's
    /// notes are pitched and stop when they end; a one-shot rings out however short the note
    /// is, and only its notes at the sampler's own pitch sound at all, because a row of boxes
    /// cannot show any others.
    ///
    /// Per pattern, like the rest of the mixer: the same sound can hold down a rhythm in one
    /// and play a melody in the next.
    SetPatternPitched {
        pattern: u16,
        track: u16,
        pitched: bool,
    },
    /// How many steps a pattern is. Applies to the clock straight away if that pattern is
    /// the one playing.
    SetPatternSteps {
        pattern: u16,
        steps: u32,
    },
    /// Add or replace the note at this step and pitch, in one pattern.
    SetNote {
        pattern: u16,
        track: u16,
        note: EngineNote,
    },
    ClearNote {
        pattern: u16,
        track: u16,
        step: u16,
        pitch: u8,
    },
    /// Forget one track's notes in one pattern.
    ClearNotes {
        pattern: u16,
        track: u16,
    },
    /// Forget everything in a pattern, for a pattern that has been deleted.
    ClearPattern {
        pattern: u16,
    },
    /// The pattern the editor has open. It is what plays, on a loop, in pattern mode.
    SetActivePattern(u16),
    /// True to play the song, false to loop the open pattern. The UI ties this to which
    /// view you are looking at.
    SetSongMode(bool),
    /// How long the song is, in steps.
    SetSongLen(u32),
    /// Forget the whole song. Sent before a project's placements go across.
    ClearSong,
    /// Play this pattern from this step of the song, for this many steps. A block longer
    /// than its pattern repeats it; a shorter one cuts it off.
    PlacePattern {
        pattern: u16,
        step: u32,
        length: u32,
    },
    /// And take it out again.
    UnplacePattern {
        pattern: u16,
        step: u32,
    },
    /// Jump the song to a step and play from there.
    SeekSong(u32),
    /// Play a track's sample right now, for clicking a row.
    Audition {
        track: u16,
        pitch: u8,
        velocity: u8,
    },
    /// Play a sample that belongs to no track, for clicking the browser.
    Preview {
        sample: Arc<Sample>,
        gain: f32,
    },
    /// Fade everything out. The panic button.
    StopAll,
}

/// Audio thread to app thread: things whose destructors must not run on the audio thread.
///
/// Dropping the last `Arc<Sample>` frees a few megabytes, and `free` can take a lock. So
/// the audio thread hands ownership back and the app thread drops it at its leisure.
#[derive(Debug)]
pub enum Trash {
    Sample(Arc<Sample>),
    /// A plugin taken off a track. Its buffers are a good few kilobytes and letting go of it
    /// may be letting go of the whole instance, so it goes home to be dropped.
    Plugin(Box<Slot>),
}

/// Commands the ring buffer holds before the app thread has to wait. A callback drains the
/// lot, so this only has to cover one burst. Opening a project sends one per note in the
/// whole song, which is more than fits: the app thread waits for room in that one case,
/// which it can afford to do and the audio thread never notices.
pub const COMMAND_CAPACITY: usize = 4096;

/// Room for returned samples. Overflowing means dropping on the audio thread, which is
/// counted in [`crate::Shared::dropped_on_audio_thread`].
pub const TRASH_CAPACITY: usize = 512;

/// Wraps the return queue so the audio thread can hand things back without caring whether
/// anyone is listening.
pub struct TrashBin {
    tx: rtrb::Producer<Trash>,
    shared: Arc<crate::Shared>,
}

impl TrashBin {
    pub fn new(tx: rtrb::Producer<Trash>, shared: Arc<crate::Shared>) -> Self {
        TrashBin { tx, shared }
    }

    /// Hand a sample back to the app thread. If the queue is full the `Arc` is dropped
    /// here, which is only a real cost when it was the last reference — and it never is,
    /// because the sample cache on the other side holds one for as long as the sample is
    /// loaded. The count still gets recorded so the queue can be sized honestly.
    #[inline]
    pub fn put(&mut self, sample: Arc<Sample>) {
        if self.tx.push(Trash::Sample(sample)).is_err() {
            self.shared.note_dropped();
        }
    }

    /// And a plugin. Same deal, except that dropping one here really would free memory, so
    /// a full queue is worse than it is for a sample.
    #[inline]
    pub fn put_plugin(&mut self, slot: Box<Slot>) {
        if self.tx.push(Trash::Plugin(slot)).is_err() {
            self.shared.note_dropped();
        }
    }
}

impl std::fmt::Debug for TrashBin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TrashBin")
    }
}
