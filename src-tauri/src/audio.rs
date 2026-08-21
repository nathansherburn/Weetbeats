//! Getting the engine onto a real audio device.
//!
//! The device callback runs on a thread the operating system schedules for real time. All
//! this module does is hand the engine to it, convert the sample format on the way out,
//! and stay out of the way.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, FromSample, OutputCallbackInfo, SampleFormat, SizedSample, StreamConfig};
use rtrb::Consumer;
use weetbeats_engine::command::TrashBin;
use weetbeats_engine::{Command, Engine, Shared};

/// Frames of f32 scratch kept for format conversion. Bigger than any sane callback, and
/// anything bigger still gets rendered in several passes rather than allocating.
const SCRATCH_FRAMES: usize = 4096;

/// What the device turned out to be. Printed once at startup: it never changes, and it is
/// only interesting when something is wrong.
#[derive(Clone, Debug)]
pub struct AudioInfo {
    pub device: String,
    pub sample_rate: u32,
    pub channels: u16,
    pub format: String,
}

/// Start the audio device and hand it the engine.
///
/// The returned info describes the device that was actually opened. The stream itself is
/// owned by a thread that does nothing else, because dropping a `cpal::Stream` stops the
/// audio and `Stream` cannot be moved into Tauri's shared state.
pub fn spawn(
    rx: Consumer<Command>,
    trash: TrashBin,
    shared: Arc<Shared>,
    errors: Arc<AtomicU32>,
    bpm: f32,
    steps: u32,
) -> Result<AudioInfo, String> {
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();

    std::thread::Builder::new()
        .name("weetbeats-audio-owner".into())
        .spawn(move || match open(rx, trash, shared, errors, bpm, steps) {
            Ok((stream, info)) => {
                if ready_tx.send(Ok(info)).is_err() {
                    return;
                }
                // Hold the stream for the life of the app. Dropping it stops the sound.
                let _stream = stream;
                loop {
                    std::thread::park();
                }
            }
            Err(e) => {
                let _ = ready_tx.send(Err(e));
            }
        })
        .map_err(|e| format!("could not start the audio thread: {e}"))?;

    ready_rx
        .recv()
        .map_err(|_| "the audio thread stopped before it started".to_string())?
}

fn open(
    rx: Consumer<Command>,
    trash: TrashBin,
    shared: Arc<Shared>,
    errors: Arc<AtomicU32>,
    bpm: f32,
    steps: u32,
) -> Result<(cpal::Stream, AudioInfo), String> {
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or_else(|| "no audio output device".to_string())?;
    let supported = device
        .default_output_config()
        .map_err(|e| format!("could not read the device config: {e}"))?;

    let format = supported.sample_format();
    let config: StreamConfig = supported.into();
    let info = AudioInfo {
        device: device.to_string(),
        sample_rate: config.sample_rate,
        channels: config.channels,
        format: format.to_string(),
    };

    // The engine is built here, on this thread, and moved into the callback. It is the
    // last allocation anything on the audio path does.
    let engine = Engine::new(config.sample_rate, bpm, steps, shared, rx, trash);

    let stream = match format {
        SampleFormat::F32 => build::<f32>(&device, config, engine, errors),
        SampleFormat::F64 => build::<f64>(&device, config, engine, errors),
        SampleFormat::I16 => build::<i16>(&device, config, engine, errors),
        SampleFormat::I32 => build::<i32>(&device, config, engine, errors),
        SampleFormat::U16 => build::<u16>(&device, config, engine, errors),
        other => Err(format!(
            "the device wants {other} samples, which we do not write"
        )),
    }?;

    stream
        .play()
        .map_err(|e| format!("could not start the stream: {e}"))?;
    Ok((stream, info))
}

fn build<T>(
    device: &Device,
    config: StreamConfig,
    mut engine: Box<Engine>,
    errors: Arc<AtomicU32>,
) -> Result<cpal::Stream, String>
where
    T: SizedSample + FromSample<f32>,
{
    let channels = config.channels as usize;
    // Preallocated, because the callback must not allocate. On macOS the device is f32 and
    // this is a straight copy; elsewhere it is where the conversion happens.
    let mut scratch = vec![0.0f32; SCRATCH_FRAMES * channels.max(1)];

    device
        .build_output_stream(
            config,
            move |data: &mut [T], _: &OutputCallbackInfo| {
                let chunk_size = scratch.len();
                for chunk in data.chunks_mut(chunk_size) {
                    let block = &mut scratch[..chunk.len()];
                    engine.render(block, channels);
                    for (slot, value) in chunk.iter_mut().zip(block.iter()) {
                        *slot = T::from_sample(*value);
                    }
                }
            },
            move |_err| {
                // This can be called from the audio thread, so it counts rather than
                // logs: printing allocates and takes a lock. The count is shown in the UI.
                errors.fetch_add(1, Ordering::Relaxed);
            },
            None,
        )
        .map_err(|e| format!("could not open the audio stream: {e}"))
}
