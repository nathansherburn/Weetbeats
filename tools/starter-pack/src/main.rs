//! Generates the drum samples that ship with Weetbeats.
//!
//! Synthesised from scratch here rather than sourced from a pack, so there is no licence
//! to honour and nothing to attribute: the app can make noise the second it opens. Run it
//! with `cargo run -p starter-pack -- assets/starter-pack`. Output is deterministic, so
//! re-running it produces byte-identical files.

use std::f32::consts::TAU;
use std::io::Write;
use std::path::Path;

const RATE: u32 = 44_100;

fn main() {
    let dir = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "assets/starter-pack".to_string());
    let dir = Path::new(&dir);
    std::fs::create_dir_all(dir).expect("could not create output folder");

    let pack: Vec<(&str, Vec<f32>)> = vec![
        ("01 kick", kick()),
        ("02 snare", snare()),
        ("03 clap", clap()),
        ("04 hat closed", hat(0.055, 0.9)),
        ("05 hat open", hat(0.34, 0.55)),
        ("06 rim", rim()),
        ("07 tom", tom()),
        ("08 cowbell", cowbell()),
    ];

    for (name, samples) in pack {
        let path = dir.join(format!("{name}.wav"));
        write_wav(&path, &samples).expect("could not write wav");
        println!("{} ({} frames)", path.display(), samples.len());
    }
}

/// Deterministic white noise, so the pack never changes between runs.
struct Noise(u32);

impl Noise {
    fn new() -> Self {
        Noise(0x5eed_1234)
    }

    fn next(&mut self) -> f32 {
        // xorshift32
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        (self.0 as f32 / u32::MAX as f32) * 2.0 - 1.0
    }
}

fn frames(secs: f32) -> usize {
    (secs * RATE as f32) as usize
}

/// Exponential decay, 1.0 down to near silence over `secs`.
fn decay(i: usize, secs: f32) -> f32 {
    (-(i as f32) / (secs * RATE as f32) * 5.0).exp()
}

/// Fade the ends so nothing starts or stops with a step change.
///
/// The fade in is deliberately shorter than the fade out. A drum is mostly transient, and
/// a long fade in files the attack off the front of it — which is why this runs before
/// normalising, not after.
fn taper(buf: &mut [f32]) {
    let n = buf.len();
    let fade_in = frames(0.001).min(n / 2);
    let fade_out = frames(0.004).min(n / 2);
    for (i, s) in buf.iter_mut().take(fade_in).enumerate() {
        *s *= i as f32 / fade_in as f32;
    }
    for i in 0..fade_out {
        buf[n - 1 - i] *= i as f32 / fade_out as f32;
    }
}

/// Scale so the loudest point sits just under full scale.
fn normalise(buf: &mut [f32], to: f32) {
    let peak = buf.iter().fold(0.0f32, |a, b| a.max(b.abs()));
    if peak > 0.0 {
        let g = to / peak;
        for s in buf.iter_mut() {
            *s *= g;
        }
    }
}

/// A one pole low pass. Cheap and enough to take the fizz off noise.
fn low_pass(buf: &mut [f32], cutoff: f32) {
    let dt = 1.0 / RATE as f32;
    let rc = 1.0 / (TAU * cutoff);
    let a = dt / (rc + dt);
    let mut last = 0.0;
    for s in buf.iter_mut() {
        last += a * (*s - last);
        *s = last;
    }
}

/// The matching high pass, for hats and snare fizz.
fn high_pass(buf: &mut [f32], cutoff: f32) {
    let dt = 1.0 / RATE as f32;
    let rc = 1.0 / (TAU * cutoff);
    let a = rc / (rc + dt);
    let mut last_in = 0.0;
    let mut last_out = 0.0;
    for s in buf.iter_mut() {
        let out = a * (last_out + *s - last_in);
        last_in = *s;
        last_out = out;
        *s = out;
    }
}

/// Sine body sweeping down in pitch, plus a click to give it a beater.
fn kick() -> Vec<f32> {
    let n = frames(0.6);
    let mut buf = vec![0.0; n];
    let mut phase = 0.0f32;
    for (i, s) in buf.iter_mut().enumerate() {
        // 110Hz down to 45Hz in the first 40ms is what makes it thump rather than beep.
        let f = 45.0 + 65.0 * (-(i as f32) / (0.04 * RATE as f32)).exp();
        phase += TAU * f / RATE as f32;
        let body = phase.sin() * decay(i, 0.42);
        let click = (-(i as f32) / (0.002 * RATE as f32)).exp() * 0.5;
        *s = body + click;
    }
    taper(&mut buf);
    normalise(&mut buf, 0.95);
    buf
}

/// Two tuned bodies for the drum, noise for the wires underneath.
fn snare() -> Vec<f32> {
    let n = frames(0.35);
    let mut noise_buf = vec![0.0; n];
    let mut rng = Noise::new();
    for (i, s) in noise_buf.iter_mut().enumerate() {
        *s = rng.next() * decay(i, 0.18);
    }
    high_pass(&mut noise_buf, 900.0);

    let mut buf = vec![0.0; n];
    for (i, s) in buf.iter_mut().enumerate() {
        let t = i as f32 / RATE as f32;
        let body = ((TAU * 185.0 * t).sin() * 0.6 + (TAU * 331.0 * t).sin() * 0.4) * decay(i, 0.09);
        *s = body * 0.7 + noise_buf[i] * 0.9;
    }
    taper(&mut buf);
    normalise(&mut buf, 0.9);
    buf
}

/// Three quick noise bursts and a room tail. That gap is what makes a clap a clap.
fn clap() -> Vec<f32> {
    let n = frames(0.4);
    let mut buf = vec![0.0; n];
    let mut rng = Noise::new();
    let bursts = [0.0f32, 0.011, 0.023];
    for (i, s) in buf.iter_mut().enumerate() {
        let t = i as f32 / RATE as f32;
        let mut env = 0.0f32;
        for start in bursts {
            if t >= start {
                env += (-(t - start) / 0.007).exp();
            }
        }
        // The tail after the last burst is the room, not another clap.
        if t >= 0.03 {
            env += (-(t - 0.03) / 0.12).exp() * 0.5;
        }
        *s = rng.next() * env;
    }
    high_pass(&mut buf, 700.0);
    low_pass(&mut buf, 7_500.0);
    taper(&mut buf);
    normalise(&mut buf, 0.85);
    buf
}

/// Metallic noise. Short decay closed, long decay open.
fn hat(secs: f32, brightness: f32) -> Vec<f32> {
    let n = frames(secs + 0.02);
    let mut buf = vec![0.0; n];
    let mut rng = Noise::new();
    for (i, s) in buf.iter_mut().enumerate() {
        let t = i as f32 / RATE as f32;
        // A few detuned partials on top of noise stops it sounding like a shaker.
        let ring = (TAU * 6_200.0 * t).sin() * 0.3
            + (TAU * 8_900.0 * t).sin() * 0.25
            + (TAU * 11_400.0 * t).sin() * 0.2;
        *s = (rng.next() * 0.8 + ring) * decay(i, secs);
    }
    high_pass(&mut buf, 6_000.0 * brightness + 2_000.0);
    taper(&mut buf);
    normalise(&mut buf, 0.7);
    buf
}

/// A stick on the rim: almost all transient.
fn rim() -> Vec<f32> {
    let n = frames(0.09);
    let mut buf = vec![0.0; n];
    let mut rng = Noise::new();
    for (i, s) in buf.iter_mut().enumerate() {
        let t = i as f32 / RATE as f32;
        let tone = (TAU * 1_720.0 * t).sin() * 0.6 + (TAU * 2_610.0 * t).sin() * 0.4;
        *s = (tone + rng.next() * 0.4) * decay(i, 0.03);
    }
    high_pass(&mut buf, 500.0);
    taper(&mut buf);
    normalise(&mut buf, 0.8);
    buf
}

/// Same idea as the kick, tuned up and left to ring.
fn tom() -> Vec<f32> {
    let n = frames(0.45);
    let mut buf = vec![0.0; n];
    let mut phase = 0.0f32;
    let mut rng = Noise::new();
    for (i, s) in buf.iter_mut().enumerate() {
        let f = 110.0 + 90.0 * (-(i as f32) / (0.06 * RATE as f32)).exp();
        phase += TAU * f / RATE as f32;
        let skin = rng.next() * (-(i as f32) / (0.004 * RATE as f32)).exp() * 0.35;
        *s = phase.sin() * decay(i, 0.3) + skin;
    }
    taper(&mut buf);
    normalise(&mut buf, 0.9);
    buf
}

/// Two clashing square waves, the way the real ones do it.
fn cowbell() -> Vec<f32> {
    let n = frames(0.3);
    let mut buf = vec![0.0; n];
    for (i, s) in buf.iter_mut().enumerate() {
        let t = i as f32 / RATE as f32;
        let a = if (TAU * 543.0 * t).sin() > 0.0 {
            1.0
        } else {
            -1.0
        };
        let b = if (TAU * 811.0 * t).sin() > 0.0 {
            1.0
        } else {
            -1.0
        };
        *s = (a * 0.5 + b * 0.5) * decay(i, 0.22);
    }
    low_pass(&mut buf, 5_000.0);
    high_pass(&mut buf, 400.0);
    taper(&mut buf);
    normalise(&mut buf, 0.75);
    buf
}

/// Minimal 16 bit mono PCM wav. No dependencies, because a header is 44 bytes.
fn write_wav(path: &Path, samples: &[f32]) -> std::io::Result<()> {
    let bytes_per_sample = 2u32;
    let data_len = samples.len() as u32 * bytes_per_sample;
    let mut out = Vec::with_capacity(44 + data_len as usize);

    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVE");

    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes()); // chunk size
    out.extend_from_slice(&1u16.to_le_bytes()); // pcm
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&RATE.to_le_bytes());
    out.extend_from_slice(&(RATE * bytes_per_sample).to_le_bytes()); // byte rate
    out.extend_from_slice(&(bytes_per_sample as u16).to_le_bytes()); // block align
    out.extend_from_slice(&16u16.to_le_bytes()); // bits

    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        let v = (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
        out.extend_from_slice(&v.to_le_bytes());
    }

    let mut file = std::fs::File::create(path)?;
    file.write_all(&out)
}
