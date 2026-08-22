//! Behaviour of the audio thread, checked by rendering into a buffer and looking at it.

use std::sync::Arc;

use weetbeats_engine::command::{TrashBin, COMMAND_CAPACITY, TRASH_CAPACITY};
use weetbeats_engine::{Command, Engine, EngineNote, Sample, Shared, Trash, MAX_VOICES};

const RATE: u32 = 48_000;

struct Rig {
    engine: Box<Engine>,
    tx: rtrb::Producer<Command>,
    trash_rx: rtrb::Consumer<Trash>,
    shared: Arc<Shared>,
}

impl Rig {
    fn new(bpm: f32, steps: u32) -> Self {
        let shared = Arc::new(Shared::new());
        let (tx, rx) = rtrb::RingBuffer::new(COMMAND_CAPACITY);
        let (trash_tx, trash_rx) = rtrb::RingBuffer::new(TRASH_CAPACITY);
        let engine = Engine::new(
            RATE,
            bpm,
            steps,
            Arc::clone(&shared),
            rx,
            TrashBin::new(trash_tx, Arc::clone(&shared)),
        );
        Rig {
            engine,
            tx,
            trash_rx,
            shared,
        }
    }

    fn send(&mut self, command: Command) {
        self.tx.push(command).expect("command queue full");
    }

    /// Render `frames` frames of stereo into a fresh buffer.
    fn render(&mut self, frames: usize) -> Vec<f32> {
        let mut out = vec![0.0f32; frames * 2];
        self.engine.render(&mut out, 2);
        out
    }

    /// Render in awkward chunks, the way a real device would.
    fn render_chunked(&mut self, frames: usize, chunk: usize) -> Vec<f32> {
        let mut out = Vec::with_capacity(frames * 2);
        let mut left = frames;
        while left > 0 {
            let n = chunk.min(left);
            out.extend_from_slice(&self.render(n));
            left -= n;
        }
        out
    }
}

/// A sample that is flat-out loud, so anything playing is obvious.
fn dc_sample(frames: usize) -> Arc<Sample> {
    Arc::new(Sample::from_data("dc", RATE, 1, vec![1.0; frames]))
}

fn track_with(rig: &mut Rig, id: u16, sample: Arc<Sample>) {
    track_with_gain(rig, id, sample, 1.0);
}

fn track_with_gain(rig: &mut Rig, id: u16, sample: Arc<Sample>, gain: f32) {
    rig.send(Command::AddTrack { track: id, gain });
    rig.send(Command::SetTrackSample {
        track: id,
        sample: Some(sample),
    });
}

fn note(step: u16) -> EngineNote {
    EngineNote {
        step,
        pitch: 60,
        velocity: 127,
        length: 1,
    }
}

/// First frame in each run of audio, i.e. where notes started.
fn onsets(out: &[f32]) -> Vec<usize> {
    let mut starts = Vec::new();
    let mut sounding = false;
    for (frame, chunk) in out.chunks(2).enumerate() {
        let loud = chunk[0].abs() > 1e-6;
        if loud && !sounding {
            starts.push(frame);
        }
        sounding = loud;
    }
    starts
}

#[test]
fn silent_until_told_to_play() {
    let mut rig = Rig::new(120.0, 16);
    track_with(&mut rig, 0, dc_sample(1000));
    rig.send(Command::SetNote {
        pattern: 0,
        track: 0,
        note: note(0),
    });
    let out = rig.render(4096);
    assert!(out.iter().all(|s| *s == 0.0), "made noise while stopped");
}

#[test]
fn steps_land_on_the_right_frame() {
    let mut rig = Rig::new(120.0, 16);
    track_with(&mut rig, 0, dc_sample(400));
    for step in [0u16, 4, 8, 12] {
        rig.send(Command::SetNote {
            pattern: 0,
            track: 0,
            note: note(step),
        });
    }
    rig.send(Command::SetPlaying(true));

    // 120bpm at 48k is 6000 frames a step. Render two bars in chunks that never line up
    // with a step boundary, so any "trigger at the top of the callback" bug shows up.
    let out = rig.render_chunked(6000 * 32, 373);
    let starts = onsets(&out);

    let expected: Vec<usize> = (0..8).map(|i| i * 4 * 6000).collect();
    assert_eq!(starts, expected, "notes did not land on their frames");
}

#[test]
fn tempo_decides_the_spacing() {
    let mut rig = Rig::new(240.0, 16);
    track_with(&mut rig, 0, dc_sample(200));
    rig.send(Command::SetNote {
        pattern: 0,
        track: 0,
        note: note(0),
    });
    rig.send(Command::SetNote {
        pattern: 0,
        track: 0,
        note: note(1),
    });
    rig.send(Command::SetPlaying(true));

    // 240bpm at 48k is 3000 frames a step.
    let out = rig.render_chunked(3000 * 4, 512);
    assert_eq!(onsets(&out)[..2], [0, 3000]);
}

#[test]
fn mute_silences_and_solo_beats_mute() {
    // Quiet tracks on purpose: at full tilt the soft clipper flattens both cases to 1.0
    // and the test would pass no matter what mute did.
    fn rig_with(muted: bool, soloed: bool) -> f32 {
        let mut rig = Rig::new(120.0, 16);
        for id in 0..2u16 {
            track_with_gain(&mut rig, id, dc_sample(6000), 0.2);
            rig.send(Command::SetNote {
                pattern: 0,
                track: id,
                note: note(0),
            });
        }
        rig.send(Command::SetTrackMuted { track: 1, muted });
        rig.send(Command::SetTrackSoloed { track: 1, soloed });
        rig.send(Command::SetPlaying(true));
        // Gain slides rather than jumps, so read the settled level, not the 10ms of ramp
        // on the way down.
        let out = rig.render(3000);
        peak(&out[2000 * 2..])
    }

    let both = rig_with(false, false);
    let muted = rig_with(true, false);
    let soloed = rig_with(true, true);

    assert!(both > 0.1, "two tracks should be audible");
    assert!(
        muted < both * 0.7,
        "mute did not quieten anything: {muted} vs {both}"
    );
    // Solo wins over the track's own mute, and silences the track that is not soloed.
    assert!(soloed > 0.1, "soloed track was silent");
    assert!(
        soloed < both * 0.7,
        "the unsoloed track was still audible: {soloed} vs {both}"
    );
}

fn peak(out: &[f32]) -> f32 {
    out.iter().fold(0.0f32, |a, b| a.max(b.abs()))
}

#[test]
fn master_never_clips_hard() {
    let mut rig = Rig::new(120.0, 16);
    // Sixteen loud tracks all on the same step is a wall of DC well past full scale.
    for id in 0..16u16 {
        track_with_gain(&mut rig, id, dc_sample(6000), 1.5);
        rig.send(Command::SetNote {
            pattern: 0,
            track: id,
            note: note(0),
        });
    }
    rig.send(Command::SetMasterGain(1.5));
    rig.send(Command::SetPlaying(true));

    let out = rig.render_chunked(24_000, 256);
    assert!(
        out.iter().all(|s| s.abs() <= 1.0),
        "output escaped the soft clipper: peak {}",
        peak(&out)
    );
    assert!(
        peak(&out) > 0.9,
        "sixteen loud tracks should be near full scale"
    );
}

#[test]
fn every_note_gets_a_voice_and_the_pool_holds() {
    let mut rig = Rig::new(120.0, 16);
    // One long sample per track, all firing every step: far more notes than voices.
    for id in 0..32u16 {
        track_with(&mut rig, id, dc_sample(48_000));
        for step in 0..16u16 {
            rig.send(Command::SetNote {
                pattern: 0,
                track: id,
                note: note(step),
            });
        }
    }
    rig.send(Command::SetPlaying(true));
    let out = rig.render_chunked(48_000, 480);

    assert!(rig.shared.playhead().active_voices <= MAX_VOICES as u32);
    assert!(out.iter().all(|s| s.is_finite()), "mixer produced NaN");
    assert!(out.iter().all(|s| s.abs() <= 1.0));
}

/// The reason voice stealing fades instead of cutting a voice dead. A jump between
/// neighbouring frames is what a click *is*, so measure it directly.
///
/// Everything here is deliberately quiet. Sixty-four DC voices at full level would sit on
/// the soft clipper, where losing one voice makes almost no difference to the output and
/// the test could not see a hard cut even if there was one.
#[test]
fn stealing_voices_does_not_click() {
    let mut rig = Rig::new(120.0, 16);
    // Eight tracks starting a note every step. The samples outlast the whole render, so
    // voices pile up: 64 of them by step 8, and every step after that has to steal.
    for id in 0..8u16 {
        track_with_gain(&mut rig, id, dc_sample(200_000), 0.015);
        for step in 0..16u16 {
            rig.send(Command::SetNote {
                pattern: 0,
                track: id,
                note: note(step),
            });
        }
    }
    rig.send(Command::SetPlaying(true));
    let out = rig.render_chunked(6000 * 16, 512);

    assert_eq!(
        rig.shared.playhead().active_voices,
        MAX_VOICES as u32,
        "the pool never filled up, so nothing was stolen and this proves nothing"
    );

    // One voice contributes about 0.0135. Eight voices fading in together climb by about
    // 0.0011 a frame. So anything past 0.005 is a voice being cut dead, not a fade.
    let (worst, at) = out
        .chunks(2)
        .map(|c| c[0])
        .collect::<Vec<_>>()
        .windows(2)
        .map(|w| (w[1] - w[0]).abs())
        .enumerate()
        .fold(
            (0.0f32, 0usize),
            |acc, (i, d)| {
                if d > acc.0 {
                    (d, i)
                } else {
                    acc
                }
            },
        );
    assert!(
        worst < 0.005,
        "jump of {worst} at frame {at} — a stolen voice was cut, not faded"
    );
}

#[test]
fn samples_go_back_to_the_app_thread_to_be_dropped() {
    let mut rig = Rig::new(120.0, 16);
    let sample = dc_sample(1000);
    track_with(&mut rig, 0, Arc::clone(&sample));
    rig.render(64);

    // Swapping the sample out must hand the old one back, not free it on the audio thread.
    rig.send(Command::SetTrackSample {
        track: 0,
        sample: None,
    });
    rig.render(64);

    let returned = std::iter::from_fn(|| rig.trash_rx.pop().ok()).count();
    assert_eq!(returned, 1, "the old sample was not handed back");
    assert_eq!(rig.shared.dropped_on_audio_thread(), 0);
    assert_eq!(
        Arc::strong_count(&sample),
        1,
        "engine still holds a reference"
    );
}

#[test]
fn deleting_a_track_stops_it() {
    let mut rig = Rig::new(120.0, 16);
    track_with(&mut rig, 0, dc_sample(48_000));
    rig.send(Command::SetNote {
        pattern: 0,
        track: 0,
        note: note(0),
    });
    rig.send(Command::SetPlaying(true));
    assert!(peak(&rig.render(3000)) > 0.1);

    rig.send(Command::RemoveTrack { track: 0 });
    // Give the release fade room to finish, then it must be properly silent.
    rig.render(1000);
    assert_eq!(peak(&rig.render(3000)), 0.0, "deleted track kept playing");
}

#[test]
fn preview_plays_without_a_track() {
    let mut rig = Rig::new(120.0, 16);
    rig.send(Command::Preview {
        sample: dc_sample(4000),
        gain: 1.0,
    });
    assert!(peak(&rig.render(2000)) > 0.1, "preview made no sound");
}

#[test]
fn stopping_rewinds_to_the_top() {
    let mut rig = Rig::new(120.0, 16);
    track_with(&mut rig, 0, dc_sample(100));
    rig.send(Command::SetPlaying(true));
    rig.render(6000 * 5);
    assert_eq!(rig.shared.playhead().step, 5);

    rig.send(Command::SetPlaying(false));
    rig.render(64);
    let playhead = rig.shared.playhead();
    assert!(!playhead.playing);
    assert_eq!(playhead.step, 0);
}

#[test]
fn pitch_changes_playback_speed() {
    // An octave up reads the sample twice as fast, so it lasts half as long.
    let mut rig = Rig::new(120.0, 16);
    track_with(&mut rig, 0, dc_sample(4800));
    rig.send(Command::Audition {
        track: 0,
        pitch: 72,
        velocity: 127,
    });
    let out = rig.render(4800);
    let sounding = out.chunks(2).filter(|c| c[0].abs() > 1e-6).count();
    assert!(
        (2200..2500).contains(&sounding),
        "octave up should last about 2400 frames, lasted {sounding}"
    );
}

#[test]
fn a_track_with_no_sample_is_harmless() {
    let mut rig = Rig::new(120.0, 16);
    rig.send(Command::AddTrack {
        track: 3,
        gain: 1.0,
    });
    rig.send(Command::SetNote {
        pattern: 0,
        track: 3,
        note: note(0),
    });
    rig.send(Command::SetPlaying(true));
    let out = rig.render(12_000);
    assert!(out.iter().all(|s| *s == 0.0));
}

#[test]
fn commands_for_slots_that_do_not_exist_are_ignored() {
    let mut rig = Rig::new(120.0, 16);
    rig.send(Command::SetTrackGain {
        track: 9_999,
        gain: 1.0,
    });
    rig.send(Command::SetNote {
        pattern: 0,
        track: 9_999,
        note: note(0),
    });
    rig.send(Command::RemoveTrack { track: 9_999 });
    rig.send(Command::SetPlaying(true));
    let out = rig.render(6000);
    assert!(out.iter().all(|s| *s == 0.0));
}

// --- patterns and the song ------------------------------------------------

fn note_in(rig: &mut Rig, pattern: u16, track: u16, step: u16) {
    rig.send(Command::SetNote {
        pattern,
        track,
        note: note(step),
    });
}

/// Hand the engine a song: how long it is in steps, and what starts where and for how long.
fn song(rig: &mut Rig, steps: u32, places: &[(u16, u32, u32)]) {
    rig.send(Command::ClearSong);
    for (pattern, step, length) in places {
        rig.send(Command::PlacePattern {
            pattern: *pattern,
            step: *step,
            length: *length,
        });
    }
    rig.send(Command::SetSongLen(steps));
    rig.send(Command::SetSongMode(true));
}

/// One step at 120bpm and 48k. Sixteenths, so four of these to a beat.
const STEP: usize = 6000;

/// One bar of the song, in frames.
const BAR: usize = STEP * 16;

#[test]
fn notes_belong_to_the_pattern_they_were_drawn_in() {
    let mut rig = Rig::new(120.0, 16);
    track_with(&mut rig, 0, dc_sample(400));
    note_in(&mut rig, 0, 0, 0);
    note_in(&mut rig, 1, 0, 8);
    rig.send(Command::SetPlaying(true));

    // Pattern 0 is the open one, so only what was drawn there sounds.
    let out = rig.render_chunked(STEP * 16, 512);
    assert_eq!(onsets(&out), vec![0]);

    rig.send(Command::SetActivePattern(1));
    let out = rig.render_chunked(STEP * 16, 512);
    assert_eq!(
        onsets(&out),
        vec![8 * STEP],
        "pattern 1 played someone else's notes"
    );
}

/// The point of placements overlapping: a kick pattern, a hat pattern and a snare pattern
/// add up to a beat.
#[test]
fn patterns_put_in_the_same_place_sound_together() {
    let mut rig = Rig::new(120.0, 16);
    track_with(&mut rig, 0, dc_sample(400));
    note_in(&mut rig, 0, 0, 0);
    note_in(&mut rig, 1, 0, 4);
    note_in(&mut rig, 2, 0, 12);
    song(&mut rig, 16, &[(0, 0, 16), (1, 0, 16), (2, 0, 16)]);
    rig.send(Command::SetPlaying(true));

    let out = rig.render_chunked(BAR, 373);
    assert_eq!(onsets(&out), vec![0, 4 * STEP, 12 * STEP]);
}

/// One placement is one play-through, however long the pattern is.
#[test]
fn a_placement_plays_the_whole_pattern_once() {
    let mut rig = Rig::new(120.0, 16);
    track_with(&mut rig, 0, dc_sample(400));
    rig.send(Command::SetPatternSteps {
        pattern: 0,
        steps: 32,
    });
    note_in(&mut rig, 0, 0, 0);
    note_in(&mut rig, 0, 0, 20);
    song(&mut rig, 32, &[(0, 0, 32)]);
    rig.send(Command::SetPlaying(true));

    let out = rig.render_chunked(STEP * 32, 373);
    assert_eq!(onsets(&out), vec![0, 20 * STEP]);
}

/// And a short pattern is not stretched to fill anything. Put a four step pattern in and you
/// get four steps of it, not a bar of it over and over.
#[test]
fn a_short_pattern_plays_once_where_it_is_put() {
    let mut rig = Rig::new(120.0, 16);
    track_with(&mut rig, 0, dc_sample(400));
    rig.send(Command::SetPatternSteps {
        pattern: 0,
        steps: 4,
    });
    note_in(&mut rig, 0, 0, 0);
    // One placement at the top of a one bar song: one hit, then twelve steps of quiet.
    song(&mut rig, 16, &[(0, 0, 4)]);
    rig.send(Command::SetPlaying(true));

    let out = rig.render_chunked(BAR, 373);
    assert_eq!(onsets(&out), vec![0]);

    // Two of them side by side is two hits, four steps apart.
    song(&mut rig, 16, &[(0, 0, 4), (0, 4, 4)]);
    rig.send(Command::SeekSong(0));
    let out = rig.render_chunked(BAR, 373);
    assert_eq!(onsets(&out), vec![0, 4 * STEP]);
}

/// Patterns of different lengths sit side by side without pushing each other about.
#[test]
fn placements_of_different_lengths_sit_side_by_side() {
    let mut rig = Rig::new(120.0, 16);
    track_with(&mut rig, 0, dc_sample(400));
    rig.send(Command::SetPatternSteps {
        pattern: 0,
        steps: 4,
    });
    rig.send(Command::SetPatternSteps {
        pattern: 1,
        steps: 12,
    });
    note_in(&mut rig, 0, 0, 0);
    note_in(&mut rig, 1, 0, 2);
    song(&mut rig, 16, &[(0, 0, 4), (1, 4, 12)]);
    rig.send(Command::SetPlaying(true));

    let out = rig.render_chunked(BAR, 373);
    // The short one at the top, then the long one starting at step four and hitting on its
    // own step two.
    assert_eq!(onsets(&out), vec![0, 6 * STEP]);
}

/// Dragging a block's edge out past the end of its pattern makes it repeat, which is what a
/// block twice as long as its pattern looks like it should do.
#[test]
fn a_block_longer_than_its_pattern_comes_round_again() {
    let mut rig = Rig::new(120.0, 16);
    track_with(&mut rig, 0, dc_sample(400));
    rig.send(Command::SetPatternSteps {
        pattern: 0,
        steps: 4,
    });
    note_in(&mut rig, 0, 0, 0);
    note_in(&mut rig, 0, 0, 2);
    // A four step pattern stretched across the bar: four times through.
    song(&mut rig, 16, &[(0, 0, 16)]);
    rig.send(Command::SetPlaying(true));

    let out = rig.render_chunked(BAR, 373);
    assert_eq!(
        onsets(&out),
        vec![
            0,
            2 * STEP,
            4 * STEP,
            6 * STEP,
            8 * STEP,
            10 * STEP,
            12 * STEP,
            14 * STEP
        ]
    );
}

/// And pulling it in cuts the pattern off part way through rather than squashing it.
#[test]
fn a_block_shorter_than_its_pattern_stops_part_way() {
    let mut rig = Rig::new(120.0, 16);
    track_with(&mut rig, 0, dc_sample(400));
    note_in(&mut rig, 0, 0, 0);
    note_in(&mut rig, 0, 0, 10);
    song(&mut rig, 16, &[(0, 0, 6)]);
    rig.send(Command::SetPlaying(true));

    let out = rig.render_chunked(BAR, 373);
    assert_eq!(onsets(&out), vec![0], "the block was over before step ten");
}

/// A row of boxes plays what the boxes show. A note the piano roll put somewhere else is
/// still in the pattern — turning the roll back on brings it back — but while the row is
/// boxes it must be silent, because nothing on screen is showing it.
#[test]
fn a_row_of_boxes_plays_only_what_the_boxes_show() {
    let mut rig = Rig::new(120.0, 16);
    track_with(&mut rig, 0, dc_sample(400));
    // A box at step nought, and a melody note somewhere the boxes cannot reach.
    note_in(&mut rig, 0, 0, 0);
    rig.send(Command::SetNote {
        pattern: 0,
        track: 0,
        note: EngineNote {
            step: 8,
            pitch: 67,
            velocity: 100,
            length: 2,
        },
    });
    rig.send(Command::SetPlaying(true));

    let out = rig.render_chunked(STEP * 16, 373);
    assert_eq!(onsets(&out), vec![0], "a hidden roll note made a sound");

    // Turn the row into a piano roll and the whole lane plays, the box included.
    rig.send(Command::SetPatternPitched {
        pattern: 0,
        track: 0,
        pitched: true,
    });
    rig.send(Command::SeekSong(0));
    rig.send(Command::SetPlaying(false));
    rig.send(Command::SetPlaying(true));
    let out = rig.render_chunked(STEP * 16, 373);
    assert_eq!(
        onsets(&out),
        vec![0, 8 * STEP],
        "the roll's own note did not come back"
    );
}

/// Being an instrument belongs to the pattern, not to the sound. The same kick can hold a
/// rhythm down in one pattern and play a melody in the next, and turning the roll off in one
/// says nothing about the other.
#[test]
fn being_an_instrument_belongs_to_the_pattern() {
    let mut rig = Rig::new(120.0, 16);
    track_with(&mut rig, 0, dc_sample(400));
    // The same note, off the sampler's own pitch, drawn in two patterns.
    for pattern in [0u16, 1] {
        rig.send(Command::SetNote {
            pattern,
            track: 0,
            note: EngineNote {
                step: 0,
                pitch: 67,
                velocity: 100,
                length: 2,
            },
        });
    }
    // A piano roll in pattern one only.
    rig.send(Command::SetPatternPitched {
        pattern: 1,
        track: 0,
        pitched: true,
    });
    rig.send(Command::SetPlaying(true));

    // Pattern nought is a row of boxes, so a note the boxes cannot show stays quiet.
    let out = rig.render_chunked(STEP * 4, 373);
    assert!(
        out.iter().all(|s| *s == 0.0),
        "the pattern with the roll turned off made a sound"
    );

    // Pattern one has the roll, so the same note plays.
    rig.send(Command::SetActivePattern(1));
    rig.send(Command::SetPlaying(false));
    rig.send(Command::SetPlaying(true));
    let out = rig.render_chunked(STEP * 4, 373);
    assert_eq!(onsets(&out), vec![0], "the roll's note did not play");
}

/// The meter in song mode, which is where it was reported dead. Nothing about it is
/// different from pattern mode — the peak is taken from the mixed output either way — and
/// this is here to say so out loud rather than leaving it to be argued about.
#[test]
fn the_meter_reads_the_song_the_same_as_a_pattern() {
    let mut rig = Rig::new(120.0, 16);
    track_with(&mut rig, 0, dc_sample(400));
    note_in(&mut rig, 0, 0, 0);
    note_in(&mut rig, 1, 0, 8);
    song(&mut rig, 32, &[(0, 0, 16), (1, 16, 16)]);
    rig.send(Command::SetPlaying(true));

    // The hit at the top of the song.
    rig.render_chunked(STEP, 256);
    let first = rig.shared.playhead().peak;
    assert!(
        first > 0.3,
        "the song's first hit did not register: {first}"
    );

    // And the one in the second bar, from the other pattern, after a stretch of quiet.
    rig.render_chunked(BAR + STEP * 7, 256);
    let quiet = rig.shared.playhead().peak;
    rig.render_chunked(STEP, 256);
    let second = rig.shared.playhead().peak;
    assert!(
        second > quiet,
        "the second pattern's hit did not move the meter: {quiet} then {second}"
    );
}

/// Seeking into a long block picks the pattern up at the right point of its repeat.
#[test]
fn seeking_into_a_repeat_lands_in_the_right_place() {
    let mut rig = Rig::new(120.0, 16);
    track_with(&mut rig, 0, dc_sample(400));
    rig.send(Command::SetPatternSteps {
        pattern: 0,
        steps: 4,
    });
    note_in(&mut rig, 0, 0, 1);
    song(&mut rig, 16, &[(0, 0, 16)]);
    rig.send(Command::SeekSong(6));
    rig.send(Command::SetPlaying(true));

    // Step six of the song is step two of the pattern's second time round, so its hit at
    // step one is three steps away.
    let out = rig.render_chunked(STEP * 4, 373);
    assert_eq!(onsets(&out), vec![3 * STEP]);
}

/// A pattern starts from its own beginning wherever it is put, not from wherever the song is.
#[test]
fn a_pattern_starts_from_the_top_of_itself() {
    let mut rig = Rig::new(120.0, 16);
    track_with(&mut rig, 0, dc_sample(400));
    note_in(&mut rig, 0, 0, 0);
    song(&mut rig, 32, &[(0, 16, 16)]);
    rig.send(Command::SetPlaying(true));

    let out = rig.render_chunked(BAR * 2, 373);
    assert_eq!(onsets(&out), vec![BAR]);
}

#[test]
fn a_gap_in_the_song_is_silence() {
    let mut rig = Rig::new(120.0, 16);
    track_with(&mut rig, 0, dc_sample(4000));
    note_in(&mut rig, 0, 0, 0);
    song(&mut rig, 48, &[(0, 0, 16), (0, 32, 16)]);
    rig.send(Command::SetPlaying(true));

    let out = rig.render_chunked(BAR * 3, 512);
    assert_eq!(onsets(&out), vec![0, BAR * 2]);
}

#[test]
fn the_song_comes_round_again_at_its_end() {
    let mut rig = Rig::new(120.0, 16);
    track_with(&mut rig, 0, dc_sample(400));
    note_in(&mut rig, 0, 0, 0);
    song(&mut rig, 32, &[(0, 0, 16)]);
    rig.send(Command::SetPlaying(true));

    // Two bars of song, so the hit at the top comes round every two bars.
    let out = rig.render_chunked(BAR * 4, 373);
    assert_eq!(onsets(&out), vec![0, BAR * 2]);
}

#[test]
fn the_playhead_says_where_in_the_song_it_is() {
    let mut rig = Rig::new(120.0, 16);
    track_with(&mut rig, 0, dc_sample(400));
    song(&mut rig, 32, &[(0, 0, 16), (2, 0, 16), (1, 16, 16)]);
    rig.send(Command::SetPlaying(true));

    rig.render_chunked(STEP * 2, 512);
    let head = rig.shared.playhead();
    assert_eq!(head.step, 2);
    assert_eq!(
        head.patterns, 0b101,
        "both patterns placed there are sounding"
    );

    // Into the second bar, where a different pattern was put.
    rig.render_chunked(STEP * 15, 512);
    let head = rig.shared.playhead();
    assert_eq!(head.step, 17);
    assert_eq!(head.patterns, 0b010);
}

#[test]
fn seeking_starts_the_song_from_that_step() {
    let mut rig = Rig::new(120.0, 16);
    track_with(&mut rig, 0, dc_sample(400));
    note_in(&mut rig, 1, 0, 0);
    song(&mut rig, 32, &[(0, 0, 16), (1, 16, 16)]);
    rig.send(Command::SeekSong(16));
    rig.send(Command::SetPlaying(true));

    // Straight in at the second bar, then round to the first again after it.
    let out = rig.render_chunked(BAR, 512);
    assert_eq!(onsets(&out), vec![0]);
    assert_eq!(rig.shared.playhead().step, 0);
}

/// Seeking into the middle of a placement picks the pattern up where it would have been,
/// rather than starting it again.
#[test]
fn seeking_into_a_placement_keeps_the_pattern_in_step() {
    let mut rig = Rig::new(120.0, 16);
    track_with(&mut rig, 0, dc_sample(400));
    rig.send(Command::SetPatternSteps {
        pattern: 0,
        steps: 32,
    });
    note_in(&mut rig, 0, 0, 20);
    song(&mut rig, 32, &[(0, 0, 32)]);
    rig.send(Command::SeekSong(16));
    rig.send(Command::SetPlaying(true));

    // Step 16 of the song is step 16 of the pattern, so its hit at step 20 is four along.
    let out = rig.render_chunked(BAR, 512);
    assert_eq!(onsets(&out), vec![4 * STEP]);
}

#[test]
fn stopping_goes_back_to_the_top_of_the_song() {
    let mut rig = Rig::new(120.0, 16);
    track_with(&mut rig, 0, dc_sample(100));
    song(&mut rig, 48, &[(0, 0, 16), (1, 16, 16), (0, 32, 16)]);
    rig.send(Command::SetPlaying(true));
    rig.render_chunked(BAR + STEP * 4, 512);
    assert_eq!(rig.shared.playhead().step, 20);

    rig.send(Command::SetPlaying(false));
    rig.render(64);
    assert_eq!(rig.shared.playhead().step, 0);
}

#[test]
fn an_empty_song_makes_no_sound() {
    let mut rig = Rig::new(120.0, 16);
    track_with(&mut rig, 0, dc_sample(4000));
    note_in(&mut rig, 0, 0, 0);
    song(&mut rig, 0, &[]);
    rig.send(Command::SetPlaying(true));

    let out = rig.render_chunked(BAR * 2, 512);
    assert!(
        out.iter().all(|s| *s == 0.0),
        "an empty song played something"
    );
}

/// A song that is cleared and built again does not bring back what used to be in it.
#[test]
fn clearing_the_song_clears_all_of_it() {
    let mut rig = Rig::new(120.0, 16);
    track_with(&mut rig, 0, dc_sample(400));
    note_in(&mut rig, 0, 0, 0);
    song(&mut rig, 48, &[(0, 0, 16), (0, 16, 16), (0, 32, 16)]);
    song(&mut rig, 48, &[(0, 0, 16)]);
    rig.send(Command::SetPlaying(true));

    let out = rig.render_chunked(BAR * 3, 512);
    assert_eq!(onsets(&out), vec![0], "an old placement came back");
}

#[test]
fn changing_a_pattern_length_changes_the_loop_under_the_playhead() {
    let mut rig = Rig::new(120.0, 16);
    track_with(&mut rig, 0, dc_sample(400));
    note_in(&mut rig, 0, 0, 0);
    rig.send(Command::SetPlaying(true));

    // Sixteen steps round: one hit.
    let out = rig.render_chunked(STEP * 16, 512);
    assert_eq!(onsets(&out).len(), 1);

    // Four steps round, without stopping: four hits in the same stretch of time.
    rig.send(Command::SetPatternSteps {
        pattern: 0,
        steps: 4,
    });
    let out = rig.render_chunked(STEP * 16, 512);
    assert_eq!(onsets(&out), vec![0, 4 * STEP, 8 * STEP, 12 * STEP]);
}

#[test]
fn a_pattern_will_not_be_longer_than_the_engine_holds() {
    let mut rig = Rig::new(120.0, 16);
    track_with(&mut rig, 0, dc_sample(400));
    rig.send(Command::SetPatternSteps {
        pattern: 0,
        steps: 9_000,
    });
    note_in(&mut rig, 0, 0, 0);
    rig.send(Command::SetPlaying(true));

    // Clamped to MAX_STEPS, so the loop comes round there rather than never.
    let out = rig.render_chunked(STEP * 520, 512);
    assert_eq!(onsets(&out), vec![0, 256 * STEP, 512 * STEP]);
}

#[test]
fn a_new_track_in_a_reused_slot_starts_with_no_notes() {
    let mut rig = Rig::new(120.0, 16);
    track_with(&mut rig, 0, dc_sample(400));
    note_in(&mut rig, 0, 0, 0);
    note_in(&mut rig, 1, 0, 0);
    rig.send(Command::RemoveTrack { track: 0 });

    // Slot 0 comes back as a different instrument. It must not inherit the old one's
    // notes, in this pattern or any other.
    track_with(&mut rig, 0, dc_sample(400));
    rig.send(Command::SetPlaying(true));
    let out = rig.render_chunked(STEP * 16, 512);
    assert!(out.iter().all(|s| *s == 0.0), "the old notes came back");

    rig.send(Command::SetActivePattern(1));
    let out = rig.render_chunked(STEP * 16, 512);
    assert!(out.iter().all(|s| *s == 0.0), "and in another pattern too");
}

#[test]
fn deleting_a_pattern_silences_it_everywhere() {
    let mut rig = Rig::new(120.0, 16);
    track_with(&mut rig, 0, dc_sample(400));
    note_in(&mut rig, 3, 0, 0);
    song(&mut rig, 16, &[(3, 0, 16)]);
    rig.send(Command::SetPlaying(true));
    assert_eq!(onsets(&rig.render_chunked(BAR, 512)), vec![0]);

    rig.send(Command::ClearPattern { pattern: 3 });
    let out = rig.render_chunked(BAR, 512);
    assert!(
        out.iter().all(|s| *s == 0.0),
        "a deleted pattern kept playing"
    );
}

#[test]
fn a_placement_past_the_end_of_what_we_hold_is_ignored() {
    let mut rig = Rig::new(120.0, 16);
    track_with(&mut rig, 0, dc_sample(400));
    note_in(&mut rig, 0, 0, 0);
    song(
        &mut rig,
        16,
        &[(0, 0, 16), (0, 900_000, 16), (9_999, 0, 16)],
    );
    rig.send(Command::SetPlaying(true));

    let out = rig.render_chunked(BAR, 512);
    assert_eq!(onsets(&out), vec![0], "the song lost its pattern");
}

// --- instruments ----------------------------------------------------------

/// Stage 3, and the audio half of stage 4: a note on an instrument stops when it ends.
#[test]
fn an_instruments_note_stops_when_it_ends() {
    let mut rig = Rig::new(120.0, 16);
    // A sample far longer than the note, so the only thing that can stop it is the note off.
    track_with(&mut rig, 0, dc_sample(200_000));
    rig.send(Command::SetPatternPitched {
        pattern: 0,
        track: 0,
        pitched: true,
    });
    rig.send(Command::SetNote {
        pattern: 0,
        track: 0,
        note: EngineNote {
            step: 0,
            pitch: 60,
            velocity: 127,
            length: 2,
        },
    });
    rig.send(Command::SetPlaying(true));

    let out = rig.render_chunked(STEP * 8, 512);
    let sounding = out.chunks(2).filter(|c| c[0].abs() > 1e-4).count();
    // Two steps, plus a few hundred frames of release so it does not click off.
    assert!(
        (2 * STEP..2 * STEP + 400).contains(&sounding),
        "a two step note lasted {sounding} frames, expected about {}",
        2 * STEP
    );
}

/// And a one-shot ignores the note's length entirely, which is what a drum wants.
#[test]
fn a_one_shot_rings_out_past_the_end_of_its_note() {
    let mut rig = Rig::new(120.0, 16);
    track_with(&mut rig, 0, dc_sample(30_000));
    rig.send(Command::SetNote {
        pattern: 0,
        track: 0,
        note: EngineNote {
            step: 0,
            pitch: 60,
            velocity: 127,
            length: 1,
        },
    });
    rig.send(Command::SetPlaying(true));

    let out = rig.render_chunked(STEP * 8, 512);
    let sounding = out.chunks(2).filter(|c| c[0].abs() > 1e-4).count();
    assert!(
        sounding > STEP * 4,
        "a one step drum hit only lasted {sounding} frames: something cut it off"
    );
}

/// Turning an instrument back into a one-shot lets it ring again.
#[test]
fn a_track_can_stop_being_an_instrument() {
    let mut rig = Rig::new(120.0, 16);
    track_with(&mut rig, 0, dc_sample(200_000));
    rig.send(Command::SetPatternPitched {
        pattern: 0,
        track: 0,
        pitched: true,
    });
    rig.send(Command::SetNote {
        pattern: 0,
        track: 0,
        note: EngineNote {
            step: 0,
            pitch: 60,
            velocity: 127,
            length: 1,
        },
    });
    rig.send(Command::SetPlaying(true));
    let held = rig.render_chunked(STEP * 4, 512);

    rig.send(Command::SetPatternPitched {
        pattern: 0,
        track: 0,
        pitched: false,
    });
    rig.send(Command::Rewind);
    let rung = rig.render_chunked(STEP * 4, 512);

    let loud = |out: &[f32]| out.chunks(2).filter(|c| c[0].abs() > 1e-4).count();
    assert!(loud(&held) < STEP * 2, "the note did not stop when held");
    assert!(loud(&rung) > STEP * 3, "the note did not ring when let go");
}

/// A note's pitch reads the sample faster or slower. The whole of stage 3.
#[test]
fn an_instrument_is_pitched_by_its_notes() {
    let mut rig = Rig::new(120.0, 16);
    track_with(&mut rig, 0, dc_sample(4_800));
    rig.send(Command::SetPatternPitched {
        pattern: 0,
        track: 0,
        pitched: true,
    });
    // An octave up, held long enough that the sample running out is what ends it.
    rig.send(Command::SetNote {
        pattern: 0,
        track: 0,
        note: EngineNote {
            step: 0,
            pitch: 72,
            velocity: 127,
            length: 16,
        },
    });
    rig.send(Command::SetPlaying(true));

    let out = rig.render_chunked(STEP * 4, 512);
    let sounding = out.chunks(2).filter(|c| c[0].abs() > 1e-4).count();
    assert!(
        (2_200..2_600).contains(&sounding),
        "an octave up should read twice as fast and last about 2400 frames, lasted {sounding}"
    );
}

/// Notes at different pitches at the same time, which is what a chord is.
#[test]
fn notes_at_different_pitches_play_together() {
    let mut rig = Rig::new(120.0, 16);
    track_with_gain(&mut rig, 0, dc_sample(200_000), 0.2);
    rig.send(Command::SetPatternPitched {
        pattern: 0,
        track: 0,
        pitched: true,
    });
    for pitch in [60u8, 64, 67] {
        rig.send(Command::SetNote {
            pattern: 0,
            track: 0,
            note: EngineNote {
                step: 0,
                pitch,
                velocity: 100,
                length: 4,
            },
        });
    }
    rig.send(Command::SetPlaying(true));

    rig.render(600);
    assert_eq!(
        rig.shared.playhead().active_voices,
        3,
        "three notes on one step should be three voices"
    );
}

/// The level meter has to be readable by something polling it sixty times a second while
/// callbacks come three times as often, so it holds the peak and slides down.
#[test]
fn the_meter_holds_a_hit_long_enough_to_be_seen() {
    let mut rig = Rig::new(120.0, 16);
    track_with(&mut rig, 0, dc_sample(200));
    note_in(&mut rig, 0, 0, 0);
    rig.send(Command::SetPlaying(true));

    // The hit is over in 200 frames. Render past it in small callbacks and the meter should
    // still be showing something a good few callbacks later.
    rig.render(256);
    let struck = rig.shared.playhead().peak;
    assert!(struck > 0.3, "the hit did not register at all: {struck}");

    rig.render_chunked(2_000, 256);
    let after = rig.shared.playhead().peak;
    assert!(
        after > 0.2,
        "the meter fell away in 40ms ({after}), so a poll would miss it"
    );

    // And it does come down eventually, rather than sticking at the top.
    rig.render_chunked(48_000, 256);
    assert!(
        rig.shared.playhead().peak < 0.05,
        "the meter never came back down"
    );
}
