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
        track: 0,
        note: note(0),
    });
    rig.send(Command::SetNote {
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
        track: 9_999,
        note: note(0),
    });
    rig.send(Command::RemoveTrack { track: 9_999 });
    rig.send(Command::SetPlaying(true));
    let out = rig.render(6000);
    assert!(out.iter().all(|s| *s == 0.0));
}
