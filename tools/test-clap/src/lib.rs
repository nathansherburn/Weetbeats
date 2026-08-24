//! A CLAP instrument the size of a postage stamp, so the host in `weetbeats-engine` has
//! something real to load.
//!
//! Hosting is the kind of code that only tells you the truth against an actual plugin: the
//! thread rules, the port declarations, the note events, the parameter events and the state
//! blob are all agreements with somebody else's binary, and a mock on our side of the line
//! would only ever agree with itself. So the tests build this, put it where a plugin lives,
//! and go through the whole of it — scan, load, activate, play a note, move a parameter, save
//! the state, load it back.
//!
//! What it does: one sine wave a note, at the pitch of the note, at a level set by its one
//! parameter. That is enough to tell "the note reached the plugin" from "it did not", "the
//! note stopped" from "it hung", and "the parameter arrived" from "it did not".
//!
//! It also has a window made of nothing: a GUI extension that goes through the whole
//! embedded-window conversation — this is the API, this is the size I want, that is the window
//! I will draw into, make it bigger please — and never draws a pixel. A test cannot look at a
//! window, but it can check that the host held up its end, and what the host did is reported
//! back out as parameters.

use std::sync::atomic::{AtomicU32, Ordering};

use clack_extensions::audio_ports::{
    AudioPortFlags, AudioPortInfo, AudioPortInfoWriter, AudioPortType, PluginAudioPorts,
    PluginAudioPortsImpl,
};
use clack_extensions::gui::{GuiConfiguration, GuiSize, HostGui, PluginGui, PluginGuiImpl, Window};
use clack_extensions::note_ports::{
    NoteDialect, NoteDialects, NotePortInfo, NotePortInfoWriter, PluginNotePorts,
    PluginNotePortsImpl,
};
use clack_extensions::params::{
    ParamDisplayWriter, ParamInfo, ParamInfoFlags, ParamInfoWriter, PluginAudioProcessorParams,
    PluginMainThreadParams, PluginParams,
};
use clack_extensions::state::{PluginState, PluginStateImpl};
use clack_extensions::timer::{HostTimer, PluginTimer, PluginTimerImpl, TimerId};
use clack_plugin::events::event_types::{NoteOffEvent, NoteOnEvent, ParamValueEvent};
use clack_plugin::events::Match;
use clack_plugin::prelude::*;
use clack_plugin::stream::{InputStream, OutputStream};
use std::io::{Read, Write};

/// The one parameter: how loud the tone is. Kept as bits in an atomic so the audio thread can
/// write it and the main thread can read it, which is what a real plugin does too.
pub struct Shared {
    level: AtomicU32,
    /// How many times the host has called our timer. Read back out as a parameter, so a test
    /// can watch it climb without needing a window to look at.
    ticks: AtomicU32,
    /// How wide the host last said our window is, or zero if it has never said. Also a
    /// parameter, and the only way a test can see that the host answered the resize we asked
    /// for.
    width: AtomicU32,
}

impl Default for Shared {
    fn default() -> Self {
        Shared {
            level: AtomicU32::new(DEFAULT_LEVEL.to_bits()),
            ticks: AtomicU32::new(0),
            width: AtomicU32::new(0),
        }
    }
}

const DEFAULT_LEVEL: f32 = 0.5;
const LEVEL_PARAM: u32 = 0;
/// A read-only parameter reporting how many times our timer has been called. Not a thing a
/// real plugin would expose; it is how the test sees the host's timers working, which is
/// otherwise only visible in a window repainting.
const TICKS_PARAM: u32 = 1;
/// How often we ask to be called. Faster than the host's own round, on purpose: a host is
/// allowed to slow a timer down, and this is how the test finds out that ours does.
const TIMER_MILLIS: u32 = 5;
/// How wide the host last made our window, as a read-only parameter. The other half of the
/// window conversation a test can see: the host is told what size we want and this is what it
/// came back and said the window is.
const WIDTH_PARAM: u32 = 2;
/// The size we say our window wants to be, the first time the host asks.
const PAPER_SIZE: GuiSize = GuiSize {
    width: 320,
    height: 240,
};
/// And the size we ask for once the window is up, which is what a plugin's own zoom control
/// does. Different from [`PAPER_SIZE`] so a test can tell which of the two it is looking at.
const ZOOMED_SIZE: GuiSize = GuiSize {
    width: 480,
    height: 360,
};

impl Shared {
    fn level(&self) -> f32 {
        f32::from_bits(self.level.load(Ordering::Relaxed))
    }

    fn set_level(&self, value: f32) {
        self.level
            .store(value.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    }
}

impl PluginShared<'_> for Shared {}

pub struct TestTone;

impl Plugin for TestTone {
    type AudioProcessor<'a> = Tone<'a>;
    type Shared<'a> = Shared;
    type MainThread<'a> = MainThread<'a>;

    fn declare_extensions(builder: &mut PluginExtensions<Self>, _shared: Option<&Shared>) {
        builder
            .register::<PluginAudioPorts>()
            .register::<PluginNotePorts>()
            .register::<PluginParams>()
            .register::<PluginState>()
            .register::<PluginTimer>()
            .register::<PluginGui>();
    }
}

impl DefaultPluginFactory for TestTone {
    fn get_descriptor() -> PluginDescriptor {
        PluginDescriptor::new("com.weetbeats.test-tone", "Weetbeats Test Tone")
            .with_vendor("Weetbeats")
            .with_features([c"instrument", c"synthesizer", c"stereo"])
    }

    fn new_shared(_host: HostSharedHandle) -> Result<Shared, PluginError> {
        Ok(Shared::default())
    }

    fn new_main_thread<'a>(
        mut host: HostMainThreadHandle<'a>,
        shared: &'a Shared,
    ) -> Result<MainThread<'a>, PluginError> {
        // Ask the host to call us regularly. A real plugin does this so its window can
        // repaint; this one does it so a test can check the host is calling.
        let timer = host
            .shared()
            .get_extension::<HostTimer>()
            .and_then(|timer| timer.register_timer(&mut host, TIMER_MILLIS).ok());
        Ok(MainThread {
            shared,
            host,
            timer,
            made: false,
            parented: false,
        })
    }
}

pub struct MainThread<'a> {
    shared: &'a Shared,
    host: HostMainThreadHandle<'a>,
    timer: Option<TimerId>,
    /// Whether the host has asked us to make a window, and whether it has given us one to
    /// draw into yet. Nothing is drawn either way; what matters is that a plugin is not
    /// allowed to be shown before it has somewhere to be.
    made: bool,
    parented: bool,
}

impl<'a> PluginMainThread<'a, Shared> for MainThread<'a> {}

impl PluginTimerImpl for MainThread<'_> {
    fn on_timer(&mut self, _timer_id: TimerId) {
        self.shared.ticks.fetch_add(1, Ordering::Relaxed);
    }
}

impl Drop for MainThread<'_> {
    fn drop(&mut self) {
        // Give the timer back, so the host is not left calling a plugin that has gone.
        let (Some(id), Some(timer)) = (self.timer, self.host.shared().get_extension::<HostTimer>())
        else {
            return;
        };
        let _ = timer.unregister_timer(&mut self.host, id);
    }
}

/// A window made of nothing.
///
/// Embedded only, and deliberately: a plugin with a view and no window of its own is the usual
/// case — everything built with JUCE is one — and it is the case that asks the most of a host.
/// Every step is checked rather than waved through, so a host that does them out of order or
/// leaves one out fails here instead of in front of somebody's synth.
impl PluginGuiImpl for MainThread<'_> {
    fn is_api_supported(&mut self, configuration: GuiConfiguration) -> bool {
        // Any windowing API, so long as the host makes the window.
        !configuration.is_floating
    }

    fn get_preferred_api(&mut self) -> Option<GuiConfiguration<'_>> {
        None
    }

    fn create(&mut self, configuration: GuiConfiguration) -> Result<(), PluginError> {
        if configuration.is_floating {
            return Err(PluginError::Message("we have no window to float"));
        }
        self.made = true;
        self.parented = false;
        Ok(())
    }

    fn destroy(&mut self) {
        self.made = false;
        self.parented = false;
    }

    fn set_scale(&mut self, _scale: f64) -> Result<(), PluginError> {
        Ok(())
    }

    fn get_size(&mut self) -> Option<GuiSize> {
        Some(PAPER_SIZE)
    }

    fn can_resize(&mut self) -> bool {
        true
    }

    fn set_size(&mut self, size: GuiSize) -> Result<(), PluginError> {
        self.shared.width.store(size.width, Ordering::Relaxed);
        Ok(())
    }

    fn set_parent(&mut self, window: Window) -> Result<(), PluginError> {
        if !self.made {
            return Err(PluginError::Message("we were not asked for a window first"));
        }
        // A host that hands over nothing has handed over nothing, whatever it thinks it did.
        // SAFETY: every window CLAP names is a pointer or a handle the same size as one, so
        // reading the union as a pointer is reading the bytes that are there either way. This
        // is a fixture looking for zero, not a plugin using the window.
        if unsafe { window.as_raw().specific.ptr }.is_null() {
            return Err(PluginError::Message("that is not a window"));
        }
        self.parented = true;
        Ok(())
    }

    fn set_transient(&mut self, _window: Window) -> Result<(), PluginError> {
        Err(PluginError::Message("we have no window to float"))
    }

    fn show(&mut self) -> Result<(), PluginError> {
        if !self.parented {
            return Err(PluginError::Message("we have nowhere to draw yet"));
        }
        // What a zoom control does: now that there is a window, ask for a bigger one. The
        // host is allowed to take its time over it, so nothing here waits for an answer.
        if let Some(gui) = self.host.shared().get_extension::<HostGui>() {
            let _ = gui.request_resize(&self.host.shared(), ZOOMED_SIZE.width, ZOOMED_SIZE.height);
        }
        Ok(())
    }

    fn hide(&mut self) -> Result<(), PluginError> {
        Ok(())
    }
}

/// One sounding note.
#[derive(Clone, Copy)]
struct Voice {
    key: u16,
    note_id: u32,
    phase: f32,
    step: f32,
    velocity: f32,
}

pub struct Tone<'a> {
    shared: &'a Shared,
    voices: Vec<Option<Voice>>,
    sample_rate: f32,
}

impl<'a> PluginAudioProcessor<'a, Shared, MainThread<'a>> for Tone<'a> {
    fn activate(
        _host: HostAudioProcessorHandle<'a>,
        _main_thread: &mut MainThread<'a>,
        shared: &'a Shared,
        audio_config: PluginAudioConfiguration,
    ) -> Result<Self, PluginError> {
        Ok(Tone {
            shared,
            voices: vec![None; 16],
            sample_rate: audio_config.sample_rate as f32,
        })
    }

    fn process(
        &mut self,
        _process: Process,
        mut audio: Audio,
        events: Events,
    ) -> Result<ProcessStatus, PluginError> {
        for event in events.input {
            if let Some(note) = event.as_event::<NoteOnEvent>() {
                self.start(note);
            } else if let Some(note) = event.as_event::<NoteOffEvent>() {
                self.stop(note);
            } else if let Some(param) = event.as_event::<ParamValueEvent>() {
                if param.param_id().map(|id| id.into()) == Some(LEVEL_PARAM) {
                    self.shared.set_level(param.value() as f32);
                }
            }
        }

        let level = self.shared.level();
        let frames = audio.frames_count() as usize;
        let Some(mut port) = audio.output_port(0) else {
            return Ok(ProcessStatus::Continue);
        };
        let Some(mut channels) = port.channels()?.into_f32() else {
            return Ok(ProcessStatus::Continue);
        };
        let spare = channels.channel_count();
        let Some(left) = channels.channel_mut(0) else {
            return Ok(ProcessStatus::Continue);
        };

        left[..frames].fill(0.0);
        for voice in self.voices.iter_mut().flatten() {
            for slot in left[..frames].iter_mut() {
                *slot += (voice.phase * std::f32::consts::TAU).sin() * voice.velocity * level;
                voice.phase = (voice.phase + voice.step).fract();
            }
        }

        // Every other channel is a copy of the first, so a note is heard from both speakers.
        for channel in 1..spare {
            let (first, mut rest) = channels.split_at_mut(1);
            let Some(from) = first.channel(0) else { break };
            let Some(to) = rest.channel_mut(channel - 1) else {
                break;
            };
            to[..frames].copy_from_slice(&from[..frames]);
        }

        Ok(ProcessStatus::Continue)
    }
}

/// Parameters arriving while the plugin is not processing. The host uses this when nothing
/// is playing, so a knob turned with the transport stopped still lands.
impl PluginAudioProcessorParams for Tone<'_> {
    fn flush(&mut self, input_events: &InputEvents, _output_events: &mut OutputEvents) {
        for event in input_events {
            if let Some(param) = event.as_event::<ParamValueEvent>() {
                if param.param_id().map(u32::from) == Some(LEVEL_PARAM) {
                    self.shared.set_level(param.value() as f32);
                }
            }
        }
    }
}

impl Tone<'_> {
    fn start(&mut self, note: &NoteOnEvent) {
        let key = match note.key() {
            Match::Specific(key) => key,
            Match::All => 60,
        };
        let note_id = match note.note_id() {
            Match::Specific(id) => id,
            Match::All => 0,
        };
        let hz = 440.0 * 2f32.powf((key as f32 - 69.0) / 12.0);
        let voice = Voice {
            key,
            note_id,
            phase: 0.0,
            step: hz / self.sample_rate,
            velocity: note.velocity() as f32,
        };
        let free = self.voices.iter().position(|v| v.is_none()).unwrap_or(0);
        self.voices[free] = Some(voice);
    }

    fn stop(&mut self, note: &NoteOffEvent) {
        let key = note.key();
        let note_id = note.note_id();
        for slot in self.voices.iter_mut() {
            let Some(voice) = slot else { continue };
            let same_key = matches!(key, Match::All) || key == Match::Specific(voice.key);
            let same_note =
                matches!(note_id, Match::All) || note_id == Match::Specific(voice.note_id);
            if same_key && same_note {
                *slot = None;
            }
        }
    }
}

impl PluginAudioPortsImpl for MainThread<'_> {
    fn count(&mut self, is_input: bool) -> u32 {
        // An instrument: nothing goes in, a stereo pair comes out.
        if is_input {
            0
        } else {
            1
        }
    }

    fn get(&mut self, index: u32, is_input: bool, writer: &mut AudioPortInfoWriter) {
        if is_input || index != 0 {
            return;
        }
        writer.set(&AudioPortInfo {
            id: ClapId::new(0),
            name: b"out",
            channel_count: 2,
            flags: AudioPortFlags::IS_MAIN,
            port_type: Some(AudioPortType::STEREO),
            in_place_pair: None,
        });
    }
}

impl PluginNotePortsImpl for MainThread<'_> {
    fn count(&mut self, is_input: bool) -> u32 {
        if is_input {
            1
        } else {
            0
        }
    }

    fn get(&mut self, index: u32, is_input: bool, writer: &mut NotePortInfoWriter) {
        if !is_input || index != 0 {
            return;
        }
        writer.set(&NotePortInfo {
            id: ClapId::new(0),
            name: b"notes",
            preferred_dialect: Some(NoteDialect::Clap),
            supported_dialects: NoteDialects::CLAP,
        });
    }
}

impl PluginMainThreadParams for MainThread<'_> {
    fn count(&mut self) -> u32 {
        3
    }

    fn get_info(&mut self, index: u32, writer: &mut ParamInfoWriter) {
        match index {
            0 => writer.set(&ParamInfo {
                id: ClapId::new(LEVEL_PARAM),
                flags: ParamInfoFlags::IS_AUTOMATABLE,
                cookie: Default::default(),
                name: b"Level",
                module: b"Tone",
                min_value: 0.0,
                max_value: 1.0,
                default_value: DEFAULT_LEVEL as f64,
            }),
            1 => writer.set(&ParamInfo {
                id: ClapId::new(TICKS_PARAM),
                flags: ParamInfoFlags::IS_READONLY,
                cookie: Default::default(),
                name: b"Ticks",
                module: b"Tone",
                min_value: 0.0,
                max_value: 1_000_000.0,
                default_value: 0.0,
            }),
            2 => writer.set(&ParamInfo {
                id: ClapId::new(WIDTH_PARAM),
                flags: ParamInfoFlags::IS_READONLY,
                cookie: Default::default(),
                name: b"Width",
                module: b"Window",
                min_value: 0.0,
                max_value: 100_000.0,
                default_value: 0.0,
            }),
            _ => {}
        }
    }

    fn get_value(&mut self, param_id: ClapId) -> Option<f64> {
        match u32::from(param_id) {
            LEVEL_PARAM => Some(self.shared.level() as f64),
            TICKS_PARAM => Some(self.shared.ticks.load(Ordering::Relaxed) as f64),
            WIDTH_PARAM => Some(self.shared.width.load(Ordering::Relaxed) as f64),
            _ => None,
        }
    }

    fn value_to_text(
        &mut self,
        param_id: ClapId,
        value: f64,
        writer: &mut ParamDisplayWriter,
    ) -> std::fmt::Result {
        use std::fmt::Write;
        match u32::from(param_id) {
            LEVEL_PARAM => write!(writer, "{}%", (value * 100.0).round() as i32),
            TICKS_PARAM => write!(writer, "{} ticks", value as u32),
            WIDTH_PARAM => write!(writer, "{} across", value as u32),
            _ => Err(std::fmt::Error),
        }
    }

    fn text_to_value(&mut self, _param_id: ClapId, text: &std::ffi::CStr) -> Option<f64> {
        text.to_str().ok()?.trim_end_matches('%').parse().ok()
    }

    fn flush(&mut self, input_events: &InputEvents, _output_events: &mut OutputEvents) {
        for event in input_events {
            if let Some(param) = event.as_event::<ParamValueEvent>() {
                if param.param_id().map(u32::from) == Some(LEVEL_PARAM) {
                    self.shared.set_level(param.value() as f32);
                }
            }
        }
    }
}

impl PluginStateImpl for MainThread<'_> {
    fn save(&mut self, output: &mut OutputStream) -> Result<(), PluginError> {
        output
            .write_all(&self.shared.level().to_le_bytes())
            .map_err(|_| PluginError::Message("could not write the level"))
    }

    fn load(&mut self, input: &mut InputStream) -> Result<(), PluginError> {
        let mut bytes = [0u8; 4];
        input
            .read_exact(&mut bytes)
            .map_err(|_| PluginError::Message("could not read the level"))?;
        self.shared.set_level(f32::from_le_bytes(bytes));
        Ok(())
    }
}

clack_export_entry!(SinglePluginEntry<TestTone>);
