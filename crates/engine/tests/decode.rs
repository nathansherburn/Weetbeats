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

/// Every extension the file picker offers has to be one we can actually read.
///
/// This is not hypothetical. `.aiff` and `.caf` were in the list the picker filters on while
/// the readers for them were switched off, so choosing an AIFF got it copied into the
/// project, failed to decode, and came back as "could not read …/samples/whatever.aiff".
/// The file the picker will let you choose and the file the decoder will take have to be the
/// same set, and the only way to know is to hand it one of each.
#[test]
fn everything_the_picker_offers_can_be_decoded() {
    let temp = std::env::temp_dir().join(format!("weetbeats-formats-{}", std::process::id()));
    std::fs::create_dir_all(&temp).unwrap();

    for (name, bytes) in [("tone.aiff", aiff()), ("tone.caf", caf())] {
        let path = temp.join(name);
        std::fs::write(&path, &bytes).unwrap();
        assert!(is_audio_file(&path), "{name} is not offered by the picker");
        let sample = decode_file(&path).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(
            sample.source_rate, 44_100,
            "{name} came back at the wrong rate"
        );
        assert_eq!(sample.channels, 1);
        assert_eq!(sample.frames, FRAMES, "{name} lost frames on the way in");
        assert!(sample.peak > 0.4, "{name} came back quiet: {}", sample.peak);
        std::fs::remove_file(&path).unwrap();
    }
    let _ = std::fs::remove_dir(&temp);
}

/// A short ramp, as sixteen bit samples. What both of the files below hold.
const FRAMES: usize = 64;

fn pcm() -> Vec<i16> {
    (0..FRAMES)
        .map(|i| ((i as f32 / FRAMES as f32) * 24_000.0) as i16)
        .collect()
}

/// A minimal AIFF: FORM/AIFF with a COMM chunk and an SSND chunk, big endian throughout.
fn aiff() -> Vec<u8> {
    let samples = pcm();
    let mut comm = Vec::new();
    comm.extend_from_slice(&1u16.to_be_bytes()); // channels
    comm.extend_from_slice(&(FRAMES as u32).to_be_bytes());
    comm.extend_from_slice(&16u16.to_be_bytes()); // bits
                                                  // 44100 as an 80 bit IEEE extended float, which is how AIFF writes a sample rate.
    comm.extend_from_slice(&[0x40, 0x0E, 0xAC, 0x44, 0, 0, 0, 0, 0, 0]);

    let mut ssnd = Vec::new();
    ssnd.extend_from_slice(&0u32.to_be_bytes()); // offset
    ssnd.extend_from_slice(&0u32.to_be_bytes()); // block size
    for sample in &samples {
        ssnd.extend_from_slice(&sample.to_be_bytes());
    }

    let mut body = Vec::new();
    body.extend_from_slice(b"AIFF");
    body.extend_from_slice(b"COMM");
    body.extend_from_slice(&(comm.len() as u32).to_be_bytes());
    body.extend_from_slice(&comm);
    body.extend_from_slice(b"SSND");
    body.extend_from_slice(&(ssnd.len() as u32).to_be_bytes());
    body.extend_from_slice(&ssnd);

    let mut out = Vec::new();
    out.extend_from_slice(b"FORM");
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    out.extend_from_slice(&body);
    out
}

/// And a minimal CAF: the Apple container, also big endian, with a desc chunk saying what
/// the data chunk holds.
fn caf() -> Vec<u8> {
    let samples = pcm();
    let mut out = Vec::new();
    out.extend_from_slice(b"caff");
    out.extend_from_slice(&1u16.to_be_bytes()); // version
    out.extend_from_slice(&0u16.to_be_bytes()); // flags

    let mut desc = Vec::new();
    desc.extend_from_slice(&44_100f64.to_be_bytes());
    desc.extend_from_slice(b"lpcm");
    desc.extend_from_slice(&2u32.to_be_bytes()); // big endian, signed integer
    desc.extend_from_slice(&2u32.to_be_bytes()); // bytes per packet
    desc.extend_from_slice(&1u32.to_be_bytes()); // frames per packet
    desc.extend_from_slice(&1u32.to_be_bytes()); // channels
    desc.extend_from_slice(&16u32.to_be_bytes()); // bits per channel
    out.extend_from_slice(b"desc");
    out.extend_from_slice(&(desc.len() as i64).to_be_bytes());
    out.extend_from_slice(&desc);

    let mut data = Vec::new();
    data.extend_from_slice(&0u32.to_be_bytes()); // edit count
    for sample in &samples {
        data.extend_from_slice(&sample.to_be_bytes());
    }
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(data.len() as i64).to_be_bytes());
    out.extend_from_slice(&data);
    out
}
