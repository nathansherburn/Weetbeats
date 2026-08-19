//! Decoded audio, ready for the audio thread to read without touching a file.

use std::fs::File;
use std::path::{Path, PathBuf};

use symphonia::core::audio::GenericAudioBufferRef;
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::codecs::CodecParameters;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;

/// Points in the little waveform the sample browser draws.
pub const WAVEFORM_POINTS: usize = 96;

/// File extensions we will try to decode.
pub const AUDIO_EXTENSIONS: &[&str] = &[
    "wav", "wave", "mp3", "flac", "ogg", "oga", "aiff", "aif", "aifc", "m4a", "mp4", "aac", "caf",
];

/// True if the path looks like something we can decode.
pub fn is_audio_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| {
            let e = e.to_ascii_lowercase();
            AUDIO_EXTENSIONS.contains(&e.as_str())
        })
        .unwrap_or(false)
}

/// A decoded sample. Cheap to clone around as an `Arc`, never mutated after decoding.
#[derive(Debug)]
pub struct Sample {
    /// File name without the extension, for showing in the UI.
    pub name: String,
    /// Where it came from, so a project can be saved with its samples later.
    pub path: PathBuf,
    /// Rate the file was recorded at. Playback rate corrects for the device rate.
    pub source_rate: u32,
    /// 1 or 2. Anything wider gets folded down to a stereo pair when decoding.
    pub channels: usize,
    /// Number of frames, i.e. `data.len() / channels`.
    pub frames: usize,
    /// Interleaved samples, `channels` wide.
    pub data: Vec<f32>,
    /// Downsampled magnitudes for drawing, always [`WAVEFORM_POINTS`] long.
    pub peaks: Vec<f32>,
    /// Loudest magnitude anywhere in the file.
    pub peak: f32,
}

impl Sample {
    /// Length in seconds at the sample's own rate.
    pub fn duration_secs(&self) -> f64 {
        if self.source_rate == 0 {
            0.0
        } else {
            self.frames as f64 / self.source_rate as f64
        }
    }

    /// Read one stereo frame at a fractional position, linearly interpolated.
    ///
    /// Positions past the end read as silence, so a voice can run off the end harmlessly.
    /// Called once per output frame per voice, so it stays branchy-but-short on purpose.
    #[inline]
    pub fn frame(&self, pos: f64) -> (f32, f32) {
        if pos < 0.0 {
            return (0.0, 0.0);
        }
        let i = pos as usize;
        if i + 1 >= self.frames {
            // Last frame gets no partner to interpolate with; hold it, then silence.
            if i >= self.frames {
                return (0.0, 0.0);
            }
            let base = i * self.channels;
            return if self.channels == 1 {
                (self.data[base], self.data[base])
            } else {
                (self.data[base], self.data[base + 1])
            };
        }
        let frac = (pos - i as f64) as f32;
        let a = i * self.channels;
        let b = a + self.channels;
        if self.channels == 1 {
            let v = self.data[a] + (self.data[b] - self.data[a]) * frac;
            (v, v)
        } else {
            let l = self.data[a] + (self.data[b] - self.data[a]) * frac;
            let r = self.data[a + 1] + (self.data[b + 1] - self.data[a + 1]) * frac;
            (l, r)
        }
    }

    /// Build a sample straight from interleaved data. Used by tests and by the tone generator.
    pub fn from_data(name: &str, source_rate: u32, channels: usize, data: Vec<f32>) -> Self {
        let channels = channels.clamp(1, 2);
        let frames = data.len() / channels;
        let (peaks, peak) = summarise(&data, channels, frames);
        Sample {
            name: name.to_string(),
            path: PathBuf::from(name),
            source_rate,
            channels,
            frames,
            data,
            peaks,
            peak,
        }
    }
}

/// Anything that can go wrong turning a file into a [`Sample`].
#[derive(Debug)]
pub enum DecodeError {
    Io(std::io::Error),
    Unsupported(String),
    Empty,
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecodeError::Io(e) => write!(f, "{e}"),
            DecodeError::Unsupported(m) => write!(f, "{m}"),
            DecodeError::Empty => write!(f, "no audio in file"),
        }
    }
}

impl std::error::Error for DecodeError {}

impl From<std::io::Error> for DecodeError {
    fn from(e: std::io::Error) -> Self {
        DecodeError::Io(e)
    }
}

impl From<symphonia::core::errors::Error> for DecodeError {
    fn from(e: symphonia::core::errors::Error) -> Self {
        match e {
            symphonia::core::errors::Error::IoError(e) => DecodeError::Io(e),
            other => DecodeError::Unsupported(other.to_string()),
        }
    }
}

/// Decode a whole file into memory. Slow, blocking, allocates — so call it from a worker
/// thread and hand the result to the audio thread inside an `Arc`.
pub fn decode_file(path: &Path) -> Result<Sample, DecodeError> {
    let file = File::open(path)?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let mut format = symphonia::default::get_probe().probe(
        &hint,
        mss,
        FormatOptions::default(),
        MetadataOptions::default(),
    )?;

    let track = format
        .default_track(TrackType::Audio)
        .or_else(|| format.first_track(TrackType::Audio))
        .ok_or_else(|| DecodeError::Unsupported("file has no audio track".into()))?;
    let track_id = track.id;
    let params = match track.codec_params.as_ref() {
        Some(CodecParameters::Audio(p)) => p.clone(),
        _ => return Err(DecodeError::Unsupported("unknown codec".into())),
    };

    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(&params, &AudioDecoderOptions::default())?;

    let mut data: Vec<f32> = Vec::new();
    let mut scratch: Vec<f32> = Vec::new();
    let mut rate = params.sample_rate.unwrap_or(44_100);
    let mut channels = params
        .channels
        .as_ref()
        .map(|c| c.count())
        .unwrap_or(2)
        .clamp(1, 2);

    loop {
        let packet = match format.next_packet() {
            Ok(Some(p)) => p,
            Ok(None) => break,
            // A truncated file is still worth playing up to where it stops.
            Err(symphonia::core::errors::Error::IoError(ref e))
                if e.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break
            }
            Err(e) => return Err(e.into()),
        };
        if packet.track_id != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            // Skip a bad packet rather than lose the whole file.
            Err(symphonia::core::errors::Error::DecodeError(_)) => continue,
            Err(e) => return Err(e.into()),
        };
        append(&decoded, &mut scratch, &mut data, &mut rate, &mut channels);
    }

    let frames = data.len() / channels;
    if frames == 0 {
        return Err(DecodeError::Empty);
    }
    data.truncate(frames * channels);

    let (peaks, peak) = summarise(&data, channels, frames);
    Ok(Sample {
        name: path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("sample")
            .to_string(),
        path: path.to_path_buf(),
        source_rate: rate,
        channels,
        frames,
        data,
        peaks,
        peak,
    })
}

/// Flatten one decoded packet onto the end of `data`, folding to at most stereo.
fn append(
    decoded: &GenericAudioBufferRef<'_>,
    scratch: &mut Vec<f32>,
    data: &mut Vec<f32>,
    rate: &mut u32,
    channels: &mut usize,
) {
    let spec = decoded.spec();
    let src_channels = spec.channels().count().max(1);
    if spec.rate() > 0 {
        *rate = spec.rate();
    }

    decoded.copy_to_vec_interleaved(scratch);
    if src_channels == *channels {
        data.extend_from_slice(scratch);
    } else if src_channels == 1 {
        // Mono packet in a stereo file: duplicate.
        for &v in scratch.iter() {
            data.push(v);
            data.push(v);
        }
    } else if *channels == 1 {
        for frame in scratch.chunks(src_channels) {
            data.push(frame.iter().sum::<f32>() / src_channels as f32);
        }
    } else {
        // More than two channels: keep the front pair, that is what a sampler wants.
        for frame in scratch.chunks(src_channels) {
            data.push(frame[0]);
            data.push(frame[1]);
        }
    }
}

/// Waveform points plus the overall peak, in one pass.
fn summarise(data: &[f32], channels: usize, frames: usize) -> (Vec<f32>, f32) {
    let mut peaks = vec![0.0f32; WAVEFORM_POINTS];
    let mut peak = 0.0f32;
    if frames == 0 {
        return (peaks, peak);
    }
    let per_point = (frames as f64 / WAVEFORM_POINTS as f64).max(1.0);
    for (i, v) in data.iter().enumerate() {
        let mag = v.abs();
        if mag > peak {
            peak = mag;
        }
        let point = ((i / channels) as f64 / per_point) as usize;
        if let Some(slot) = peaks.get_mut(point) {
            if mag > *slot {
                *slot = mag;
            }
        }
    }
    (peaks, peak)
}
