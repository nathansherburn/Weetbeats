//! Decoding real files, using the starter pack as the fixture.

use std::path::{Path, PathBuf};

use weetbeats_engine::sample::{decode_file, is_audio_file, WAVEFORM_POINTS};
use weetbeats_engine::Sample;

fn pack() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/starter-pack")
}

fn load(name: &str) -> Sample {
    decode_file(&pack().join(name)).unwrap_or_else(|e| panic!("could not decode {name}: {e}"))
}

#[test]
fn decodes_the_starter_pack() {
    let mut found = 0;
    for entry in std::fs::read_dir(pack()).expect("starter pack is missing") {
        let path = entry.unwrap().path();
        if !is_audio_file(&path) {
            continue;
        }
        found += 1;
        let sample = decode_file(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        assert_eq!(sample.source_rate, 44_100);
        assert_eq!(sample.channels, 1);
        assert!(
            sample.frames > 1_000,
            "{} is suspiciously short",
            sample.name
        );
        assert!(sample.data.iter().all(|s| s.is_finite()));
        assert!(
            (0.5..=1.0).contains(&sample.peak),
            "{} peaks at {}",
            sample.name,
            sample.peak
        );
        assert_eq!(sample.peaks.len(), WAVEFORM_POINTS);
    }
    assert_eq!(found, 8, "expected eight samples in the pack");
}

#[test]
fn names_come_from_the_file_name() {
    let sample = load("01 kick.wav");
    assert_eq!(sample.name, "01 kick");
}

#[test]
fn drums_land_where_a_drum_should() {
    // Rough sanity on the synthesis: a kick's energy is low, a hat's is high. Measured as
    // zero crossings a second, which is a crude pitch estimate and plenty for this.
    let rate = |s: &Sample| {
        let crossings = s
            .data
            .windows(2)
            .filter(|w| (w[0] < 0.0) != (w[1] < 0.0))
            .count();
        crossings as f64 / s.duration_secs()
    };
    let kick = rate(&load("01 kick.wav"));
    let hat = rate(&load("04 hat closed.wav"));
    assert!(kick < 400.0, "kick is not low: {kick} crossings a second");
    assert!(hat > 4_000.0, "hat is not bright: {hat} crossings a second");
}

#[test]
fn interpolated_reads_stay_inside_the_file() {
    let sample = load("02 snare.wav");
    // Reading anywhere, including well past the end, must be silent rather than a panic.
    for pos in [0.0, 0.5, 1.5, 100.25, (sample.frames - 1) as f64] {
        let (l, r) = sample.frame(pos);
        assert!(l.is_finite() && r.is_finite());
    }
    assert_eq!(sample.frame(sample.frames as f64), (0.0, 0.0));
    assert_eq!(sample.frame(1e9), (0.0, 0.0));
    assert_eq!(sample.frame(-1.0), (0.0, 0.0));
}

#[test]
fn mono_reads_the_same_on_both_sides() {
    let sample = load("01 kick.wav");
    let (l, r) = sample.frame(120.5);
    assert_eq!(l, r);
    assert_ne!(l, 0.0);
}

#[test]
fn interpolation_sits_between_its_neighbours() {
    let sample = Sample::from_data("ramp", 44_100, 1, vec![0.0, 1.0, 0.0]);
    assert_eq!(sample.frame(0.0).0, 0.0);
    assert_eq!(sample.frame(0.5).0, 0.5);
    assert_eq!(sample.frame(1.0).0, 1.0);
    assert_eq!(sample.frame(1.25).0, 0.75);
}

#[test]
fn rejects_things_that_are_not_audio() {
    let path = pack().join("../../README.md");
    assert!(!is_audio_file(&path));
    assert!(decode_file(&path).is_err());
}

#[test]
fn extension_matching_ignores_case() {
    assert!(is_audio_file(Path::new("/x/Kick.WAV")));
    assert!(is_audio_file(Path::new("/x/loop.Flac")));
    assert!(!is_audio_file(Path::new("/x/notes.txt")));
    assert!(!is_audio_file(Path::new("/x/no-extension")));
}
