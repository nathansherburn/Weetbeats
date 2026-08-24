//! Hosting CLAP instrument plugins, so a track's sound can be Surge XT instead of a sample.
//!
//! ## Three threads, and which one owns what
//!
//! CLAP is explicit about threads, and so is this.
//!
//! * The **plugin thread** — [`Desk`] — owns every loaded [`PluginInstance`]. CLAP calls this
//!   the main thread: scanning, loading, reading parameters and saving state all happen here
//!   and nowhere else. It is a thread of its own rather than the app's, because a plugin
//!   instance is `!Send` and has to stay put, and because loading Surge XT takes long enough
//!   that doing it on the window's thread would freeze the window.
//! * The **audio thread** owns the [`Slot`]s: the plugin's audio processor, its buffers, and
//!   the events going into it. Everything a slot needs is allocated on the app thread and
//!   handed over, the same way samples are, so [`Engine::render`](crate::Engine::render) keeps
//!   its promise not to allocate.
//! * The **app thread** asks the desk for things and passes the results on. It never touches
//!   a plugin directly.
//!
//! ## What a plugin track is
//!
//! A sound source, and only that. Notes come from the pattern exactly as they do for a
//! sampler; the plugin turns them into audio and the mixer treats what comes out like any
//! other track.
//!
//! One thing does work differently and it is worth being plain about. A sampler gives every
//! note its own voice, so a pattern's fader can be applied to the notes *that pattern*
//! started, even while several patterns play the same sound at once. A plugin makes one
//! sound for the whole track: there is nothing to apply a per-pattern fader to on the way
//! out. So a pattern's fader and mute are applied to the notes it *sends* — a quieter fader
//! sends quieter notes, a muted pattern sends none — and the level in the sound editor is
//! applied to what comes out. Turning a fader down while a note rings does not change that
//! note; the next one is quieter.
//!
//! ## The plugin's own window
//!
//! Whichever of the two ways the plugin will have. A floating window is the plugin's own —
//! it makes it, sizes it, draws it and closes it, and all the host does is say which window
//! it should stay above and what to call itself. An embedded one is drawn into a window the
//! host makes, which then has to be sized to fit and resized when the plugin asks.
//!
//! Floating is preferred because it is the plugin doing all of it, but preference is as far
//! as it goes: a plugin says which it supports and a great many support only embedding.
//! Anything built with JUCE — Surge XT among them — has a view and no window, so embedding is
//! not the exotic case, it is the usual one.
//!
//! A window is what pins the studio to a particular thread. CLAP lets a host pick which thread
//! it calls the main thread — but a window is made of the platform's own widgets, and macOS
//! only makes those on the process's first thread. So a [`Studio`] that will ever open a
//! window has to live there. [`Desk`] puts one on a thread of its own, which is right for
//! anything that will not.
//!
//! Two of the host's own extensions come with the window. One is how we hear that somebody
//! closed it; the other keeps the timers a GUI repaints on, fired from [`Studio::tick`].
//!
//! ## What is not here yet
//!
//! Plugins run in this process rather than a child one, so a plugin that crashes takes the app
//! with it. That is the other half of hosting properly, and a bigger piece of work than the
//! window was.

use std::ffi::CString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::{Duration, Instant};

use clack_extensions::audio_ports::{AudioPortInfoBuffer, PluginAudioPorts};
use clack_extensions::gui::{
    GuiApiType, GuiConfiguration, GuiSize, HostGui, HostGuiImpl, PluginGui, Window,
};
use clack_extensions::params::{ParamInfoBuffer, ParamInfoFlags, PluginParams};
use clack_extensions::state::PluginState;
use clack_extensions::timer::{HostTimer, HostTimerImpl, PluginTimer, TimerId};
use clack_host::events::event_types::{NoteOffEvent, NoteOnEvent, ParamValueEvent};
use clack_host::events::io::{EventBuffer, InputEvents, OutputEvents};
use clack_host::events::{Match, Pckn};
use clack_host::prelude::*;
use clack_host::process::audio_buffers::{AudioPortBuffer, AudioPortBufferType, InputChannel};
use clack_host::process::PluginAudioProcessor;
use clack_host::utils::{ClapId, Cookie};
use serde::Serialize;

use crate::{MAX_BLOCK, MAX_TRACKS};

/// Notes one plugin track can have ringing at once. Past this the oldest is let go of, which
/// only ever happens on a track being played far harder than a pattern can play it.
pub const MAX_PLUGIN_NOTES: usize = 32;

/// Events one block can carry into a plugin: every note starting and stopping, plus room for
/// parameter changes arriving from the window while it plays.
const EVENT_CAPACITY: usize = MAX_PLUGIN_NOTES * 2 + 64;

/// The most channels one of a plugin's ports may have, and the most ports it may have each
/// way, before we decline to host it.
///
/// A host has to hand a plugin exactly the ports it declared — one too few, or one a channel
/// short, and the plugin writes past the end of a buffer that is not there. So these are not
/// a clamp: a plugin past them is refused, with a message, rather than played with the wrong
/// shape. Sixteen and eight are far past anything a synth asks for.
const MAX_PORT_CHANNELS: usize = 16;
const MAX_PORTS: usize = 8;

// --- what the host says about itself ----------------------------------------

/// What a plugin has told us about its window since we last looked.
#[derive(Clone, Copy, PartialEq, Eq)]
enum WindowNews {
    /// The window has gone and the plugin has already taken it apart. CLAP says we have to
    /// call `destroy` to say we noticed.
    Destroyed,
    /// The window has gone but the plugin is still holding it.
    Closed,
}

/// What a plugin can ask the host to do, which it can do from any thread.
///
/// All noted rather than acted on. Some of these arrive while the audio thread is inside
/// `process`, and none of them can be done from there, so every one is a flag the main thread
/// picks up next time round.
#[derive(Default)]
pub struct Shared {
    /// The plugin wants `on_main_thread` called.
    wants_callback: AtomicBool,
    /// The plugin wants deactivating and activating again. Noted so it can be reported; we
    /// do not restart plugins mid-song.
    wants_restart: AtomicBool,
    /// Its window has gone, and whether it took the window with it.
    window_gone: AtomicBool,
    window_destroyed: AtomicBool,
    /// The size an embedded window has been asked to become, both halves in the one atomic so
    /// a width can never be read against somebody else's height. Zero for nothing asked.
    wanted_size: AtomicU64,
}

impl Shared {
    /// What has happened to the plugin's window since last time, once.
    fn take_window_news(&self) -> Option<WindowNews> {
        if !self.window_gone.swap(false, Ordering::Relaxed) {
            return None;
        }
        Some(match self.window_destroyed.swap(false, Ordering::Relaxed) {
            true => WindowNews::Destroyed,
            false => WindowNews::Closed,
        })
    }

    /// The size the plugin last asked its window to be, once.
    fn take_wanted_size(&self) -> Option<GuiSize> {
        match self.wanted_size.swap(0, Ordering::Relaxed) {
            0 => None,
            packed => Some(GuiSize::unpack_from_u64(packed)),
        }
    }
}

impl SharedHandler<'_> for Shared {
    fn request_restart(&self) {
        self.wants_restart.store(true, Ordering::Relaxed);
    }

    fn request_process(&self) {
        // We process every block regardless, so there is nothing to turn on.
    }

    fn request_callback(&self) {
        self.wants_callback.store(true, Ordering::Relaxed);
    }
}

/// The host's half of the window arrangement.
///
/// Two of these matter. [`closed`](HostGuiImpl::closed) is how we hear that somebody clicked
/// the close box on a window the plugin drew; [`request_resize`](HostGuiImpl::request_resize)
/// is how an embedded plugin asks for the window it is drawing into to change size, which is
/// what Surge XT's zoom menu does. Both are noted rather than acted on: they arrive from
/// whichever thread the plugin felt like, and both are jobs for the main one.
impl HostGuiImpl for Shared {
    fn resize_hints_changed(&self) {
        // Its aspect ratio and step size, which only matter while a window is being dragged
        // to a new size — and ours are not draggable yet.
    }

    fn request_resize(&self, new_size: GuiSize) -> Result<(), HostError> {
        // Accepted here and done on the next trip round the main thread: the window is ours
        // to resize, and this is not the thread that can.
        self.wanted_size
            .store(new_size.pack_to_u64(), Ordering::Relaxed);
        Ok(())
    }

    fn request_show(&self) -> Result<(), HostError> {
        // Showing a floating window is the plugin's own to do; an embedded one is already up
        // by the time it can ask, because we open the window before we hand it over.
        Ok(())
    }

    fn request_hide(&self) -> Result<(), HostError> {
        Ok(())
    }

    fn closed(&self, was_destroyed: bool) {
        self.window_destroyed
            .store(was_destroyed, Ordering::Relaxed);
        self.window_gone.store(true, Ordering::Relaxed);
    }
}

/// Timers one plugin may have running at once. A GUI wants one; a few want two or three.
const MAX_TIMERS: usize = 8;

/// One timer a plugin has asked us to keep for it.
#[derive(Clone, Copy)]
struct Timer {
    id: TimerId,
    period: Duration,
    due: Instant,
}

/// The host's main thread, which for a plugin means two things: somewhere to keep its timers,
/// and somebody to call them.
///
/// Timers are how a GUI made of somebody else's widgets gets to repaint itself, so a plugin
/// with a window almost certainly registers one. They are fired from [`Studio::tick`], which
/// runs about sixty times a second — CLAP asks a host to allow at least thirty, and lets it
/// slow anything faster down, which this does by simply not being able to fire more often.
#[derive(Default)]
pub struct MainThread {
    timers: Vec<Timer>,
    next_timer: u32,
}

impl MainThread {
    /// Every timer that has come due, written into `due`, and how many there were. Each one is
    /// wound on from now rather than from when it was meant to fire, so a slow frame does not
    /// leave a backlog to catch up on.
    fn timers_due(&mut self, now: Instant, due: &mut [TimerId; MAX_TIMERS]) -> usize {
        let mut count = 0;
        for timer in self.timers.iter_mut() {
            if timer.due > now || count >= due.len() {
                continue;
            }
            due[count] = timer.id;
            count += 1;
            timer.due = now + timer.period;
        }
        count
    }
}

impl MainThreadHandler<'_> for MainThread {}

impl HostTimerImpl for MainThread {
    fn register_timer(&mut self, period_ms: u32) -> Result<TimerId, HostError> {
        if self.timers.len() >= MAX_TIMERS {
            return Err(HostError::Message("that is more timers than we will keep"));
        }
        let id = TimerId(self.next_timer);
        self.next_timer += 1;
        // Nothing faster than the poll that fires them, because pretending otherwise would
        // only mean a plugin thinking it was being called more often than it was.
        let period = Duration::from_millis(period_ms.max(TICK_MILLIS) as u64);
        self.timers.push(Timer {
            id,
            period,
            due: Instant::now() + period,
        });
        Ok(id)
    }

    fn unregister_timer(&mut self, timer_id: TimerId) -> Result<(), HostError> {
        let before = self.timers.len();
        self.timers.retain(|timer| timer.id != timer_id);
        match self.timers.len() == before {
            true => Err(HostError::Message("no such timer")),
            false => Ok(()),
        }
    }
}

/// How often the main thread comes round, which is what sets the floor on a timer's period.
/// Must match how often the app calls [`Studio::tick`] — it does it from the playhead poll.
const TICK_MILLIS: u32 = 16;

/// The host, as CLAP wants it: one type per thread specification.
pub struct Weetbeats;

impl HostHandlers for Weetbeats {
    type Shared<'a> = Shared;
    type MainThread<'a> = MainThread;
    type AudioProcessor<'a> = ();

    /// What we can do for a plugin that it cannot do for itself. Both of these are about
    /// windows: one to hear that a window has closed, one to keep the timers a GUI repaints on.
    fn declare_extensions(builder: &mut HostExtensions<Self>, _shared: &Shared) {
        builder.register::<HostGui>().register::<HostTimer>();
    }
}

/// The audio processor half of a plugin instance, which is the half that is `Send`.
pub type Processor = PluginAudioProcessor<Weetbeats>;

// --- finding plugins ---------------------------------------------------------

/// One plugin, as found on disk. What the picker shows.
#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Found {
    /// The `.clap` file or bundle it lives in.
    pub path: String,
    /// Its id inside that file, which is what identifies it forever after.
    pub id: String,
    pub name: String,
    pub vendor: String,
    /// False for effects and anything else that is not something you play notes on. Shown
    /// greyed out rather than hidden, so "why is my plugin not in the list" has an answer.
    pub instrument: bool,
}

/// Where CLAP plugins live, in the order a host is expected to look.
///
/// `$CLAP_PATH` comes first because somebody who has set it means it. The rest are the
/// standard places for the platform: a user's own folder, then the machine's.
pub fn search_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(set) = std::env::var_os("CLAP_PATH") {
        let separator = if cfg!(windows) { ';' } else { ':' };
        for one in set.to_string_lossy().split(separator) {
            if !one.is_empty() {
                paths.push(PathBuf::from(one));
            }
        }
    }
    let home = std::env::var_os("HOME").map(PathBuf::from);

    #[cfg(target_os = "macos")]
    {
        if let Some(home) = &home {
            paths.push(home.join("Library/Audio/Plug-Ins/CLAP"));
        }
        paths.push(PathBuf::from("/Library/Audio/Plug-Ins/CLAP"));
    }

    #[cfg(target_os = "windows")]
    {
        if let Some(common) = std::env::var_os("COMMONPROGRAMFILES") {
            paths.push(PathBuf::from(common).join("CLAP"));
        }
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            paths.push(PathBuf::from(local).join("Programs/Common/CLAP"));
        }
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if let Some(home) = &home {
            paths.push(home.join(".clap"));
        }
        paths.push(PathBuf::from("/usr/lib/clap"));
        paths.push(PathBuf::from("/usr/local/lib/clap"));
    }

    let _ = home;
    paths
}

/// Every `.clap` under a folder, a few levels down. Vendors put their plugins in a folder of
/// their own — `~/.clap/Surge Synth Team/` — so a flat listing would miss most of them.
fn clap_files(root: &Path, depth: usize, found: &mut Vec<PathBuf>) {
    const MAX_DEPTH: usize = 4;
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let is_clap = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("clap"));
        if is_clap {
            // On macOS a `.clap` is a bundle, which is a folder; everywhere else it is a
            // file. Either way it is the thing to load, and not somewhere to look inside.
            found.push(path);
        } else if depth < MAX_DEPTH && path.is_dir() {
            clap_files(&path, depth + 1, found);
        }
    }
}

// --- the plugin thread -------------------------------------------------------

/// One parameter, as the sound editor shows it.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Param {
    pub id: u32,
    pub name: String,
    /// The plugin's own grouping, e.g. "Oscillators/Wavetable 1". Used to sort and to filter.
    pub module: String,
    pub min: f64,
    pub max: f64,
    pub value: f64,
    /// What the plugin calls that value — "-6.0 dB", "Sawtooth". Read when the list is, so it
    /// is right when the editor opens and goes plain while you drag.
    pub text: String,
    /// True for a parameter that only takes whole numbers, which is a menu or a switch.
    pub stepped: bool,
}

/// What comes back when a plugin has been loaded onto a track.
pub struct Loaded {
    /// For the audio thread. Goes across in a [`Command`](crate::Command).
    pub slot: Box<Slot>,
    pub name: String,
    pub params: Vec<Param>,
}

/// What the app thread asks the plugin thread to do.
/// One of the host's windows, as the platform's windowing API names it.
///
/// Handed to a plugin either to stay above — a floating window — or to draw into, which is an
/// embedded one. Kept as a plain pointer rather than anything from a windowing crate, so this
/// module stays about CLAP: the app hands one over, and what it means is the app's business.
#[derive(Clone, Copy, Debug)]
pub enum ParentWindow {
    /// An `NSView *`, which is what macOS calls a window's contents.
    Cocoa(*mut std::ffi::c_void),
    /// An `HWND`.
    Win32(*mut std::ffi::c_void),
    /// An X11 window id.
    X11(std::os::raw::c_ulong),
}

impl ParentWindow {
    /// As CLAP wants it. `None` when the platform's API is not one CLAP has a name for.
    ///
    /// # Safety
    ///
    /// The window has to still exist for as long as the plugin's GUI does.
    unsafe fn as_clap(&self) -> Window<'static> {
        match *self {
            ParentWindow::Cocoa(view) => Window::from_cocoa_nsview(view),
            ParentWindow::Win32(hwnd) => Window::from_win32_hwnd(hwnd),
            ParentWindow::X11(handle) => Window::from_x11_handle(handle),
        }
    }
}

/// Which of the two kinds of window a plugin will have.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WindowStyle {
    /// The plugin's own window. It needs nothing from the host but somebody to float above.
    Floating,
    /// A window the host makes, which the plugin draws into. The host has to make it the right
    /// size and change that size when the plugin asks.
    Embedded {
        /// Whether the sizes either side quotes are logical pixels rather than physical ones.
        /// True on macOS, false everywhere else, and CLAP's rule rather than ours.
        logical: bool,
    },
}

/// A window size, in whichever pixels [`WindowStyle::Embedded`] said.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct WindowSize {
    pub width: u32,
    pub height: u32,
}

impl WindowSize {
    fn from_gui(size: GuiSize) -> WindowSize {
        WindowSize {
            width: size.width,
            height: size.height,
        }
    }

    fn as_gui(self) -> GuiSize {
        GuiSize {
            width: self.width,
            height: self.height,
        }
    }
}

/// Everything the plugin thread owns: the `.clap` files it has opened and the instances it
/// has made from them.
///
/// **Single threaded, and particular about which thread.** CLAP calls one thread the main
/// thread and wants every instance made, asked about and destroyed on it. A [`PluginInstance`]
/// is `!Send`, so the type system holds most of that; what it cannot say is *which* thread,
/// and that matters as soon as windows are involved. A plugin's window is made of the
/// platform's own widgets, and macOS will only make those on the process's first thread — so
/// a studio that is ever going to open a window has to live there.
///
/// [`Desk`] puts one on a thread of its own, which is right for anything that will not.
pub struct Studio {
    /// Kept for the life of the app: unloading a `.clap` while an instance from it is alive is
    /// how a host crashes, and reloading Surge XT's entry per instance is slow.
    entries: Vec<(PathBuf, PluginEntry)>,
    live: Vec<Option<Live>>,
    sample_rate: f64,
}

impl Studio {
    pub fn new(sample_rate: f64) -> Studio {
        Studio {
            entries: Vec::new(),
            live: (0..MAX_TRACKS).map(|_| None).collect(),
            sample_rate,
        }
    }

    /// Every plugin on the machine. Takes a moment: each `.clap` has to be opened to be asked
    /// what is inside it.
    pub fn scan(&mut self) -> Vec<Found> {
        scan_all(&mut self.entries)
    }

    /// Put a plugin on a track, with its saved settings if it has any.
    pub fn load(
        &mut self,
        track: u16,
        path: &Path,
        id: &str,
        state: Option<Vec<u8>>,
    ) -> Result<Loaded, String> {
        // Whatever was there goes first, so a track never has two instances alive.
        self.unload(track);
        load_one(
            &mut self.entries,
            &mut self.live,
            track,
            path,
            id,
            state,
            self.sample_rate,
        )
    }

    /// Take it off again, and destroy the instance. Its window goes with it, on the way out.
    pub fn unload(&mut self, track: u16) {
        if let Some(slot) = self.live.get_mut(track as usize) {
            *slot = None;
        }
    }

    /// What the plugin's parameters are and where they are now.
    pub fn params(&mut self, track: u16) -> Vec<Param> {
        match self.at(track) {
            Some(live) => read_params(&mut live.instance),
            None => Vec::new(),
        }
    }

    /// The plugin's own settings, as an opaque blob to write into the project folder.
    pub fn save_state(&mut self, track: u16) -> Option<Vec<u8>> {
        save_state(&mut self.at(track)?.instance)
    }

    /// Drop a slot the audio thread has finished with.
    ///
    /// A slot holds a share of the plugin instance. Whoever lets go of the last share destroys
    /// the plugin, and CLAP says that happens on the thread that made it — so it happens here,
    /// whichever order the two halves come apart in.
    pub fn bin(&mut self, slot: Box<Slot>) {
        drop(slot);
    }

    // --- the plugin's own window -------------------------------------------

    /// Which kind of window this plugin will have, so the app knows whether it has to make
    /// one. Ask this before [`open_window`](Studio::open_window), which wants the answer.
    ///
    /// Floating is preferred: it is the plugin doing all the work, and there is no host window
    /// to size, resize or close. Most plugins do not offer it, so most of the time this comes
    /// back embedded.
    pub fn window_style(&mut self, track: u16) -> Result<WindowStyle, String> {
        let Some(live) = self.at(track) else {
            return Err("there is no plugin on that track".into());
        };
        let Some(gui) = live
            .instance
            .plugin_shared_handle()
            .get_extension::<PluginGui>()
        else {
            return Err("this plugin has no window of its own".into());
        };
        let Some(api) = GuiApiType::default_for_current_platform() else {
            return Err("we have no way to show a window on this system".into());
        };
        let mut handle = live.instance.plugin_handle();
        let mut supported = |is_floating| {
            gui.is_api_supported(
                &mut handle,
                GuiConfiguration {
                    api_type: api,
                    is_floating,
                },
            )
        };
        if supported(true) {
            return Ok(WindowStyle::Floating);
        }
        if supported(false) && api.supports_embedding() {
            return Ok(WindowStyle::Embedded {
                logical: api.uses_logical_size(),
            });
        }
        Err("this plugin will not open a window of its own here".into())
    }

    /// Open the plugin's window.
    ///
    /// `window` is asked for the window to use once there is something to say about it: which
    /// kind it is going to be, and — for one the host makes — how big the plugin wants it. It
    /// hands back the window a floating one should stay above, or the one an embedded one is to
    /// be drawn into. `None` from it is fine for a floating window and fatal for an embedded
    /// one, because a plugin with nowhere to draw has no window.
    ///
    /// The order is CLAP's and it matters: a plugin is asked what size it wants *before* it is
    /// given the window, so the window can be made that size to begin with. Handing over a
    /// window of the wrong size and resizing it afterwards leaves the plugin's view where the
    /// old size put it.
    ///
    /// # Safety
    ///
    /// Whatever `window` hands back has to still exist for as long as the plugin's window
    /// does, which is until [`close_window`](Studio::close_window) or the plugin going.
    pub unsafe fn open_window<F>(
        &mut self,
        track: u16,
        title: &str,
        window: F,
    ) -> Result<WindowStyle, String>
    where
        F: FnOnce(WindowStyle, Option<WindowSize>) -> Option<ParentWindow>,
    {
        let style = self.window_style(track)?;
        let api = GuiApiType::default_for_current_platform()
            .ok_or_else(|| "we have no way to show a window on this system".to_string())?;
        let live = self
            .at(track)
            .ok_or_else(|| "there is no plugin on that track".to_string())?;
        let gui = live
            .instance
            .plugin_shared_handle()
            .get_extension::<PluginGui>()
            .ok_or_else(|| "this plugin has no window of its own".to_string())?;
        if live.window {
            return Ok(style);
        }

        let mut handle = live.instance.plugin_handle();
        gui.create(
            &mut handle,
            GuiConfiguration {
                api_type: api,
                is_floating: matches!(style, WindowStyle::Floating),
            },
        )
        .map_err(|e| format!("its window would not open: {e}"))?;

        // What the plugin would like the window to be, asked before there is one. A plugin
        // that will not say gets whatever the caller makes.
        let wanted = match style {
            WindowStyle::Floating => None,
            WindowStyle::Embedded { .. } => gui.get_size(&mut handle).map(WindowSize::from_gui),
        };
        let parent = window(style, wanted);

        match style {
            WindowStyle::Floating => {
                // Both of these are hints a plugin is allowed to ignore, so neither is worth
                // failing over: a window in the wrong place with the wrong name still beats no
                // window.
                if let Some(parent) = parent {
                    // SAFETY: the caller promises the window outlives the plugin's GUI.
                    let _ = unsafe { gui.set_transient(&mut handle, parent.as_clap()) };
                }
                if let Ok(title) = CString::new(title) {
                    gui.suggest_title(&mut handle, &title);
                }
            }
            WindowStyle::Embedded { .. } => {
                // This one is not a hint. A plugin that will not take the window it is given
                // has nowhere to draw, so there is nothing to show and the GUI goes back.
                let Some(parent) = parent else {
                    gui.destroy(&mut handle);
                    return Err(
                        "this plugin draws into a window, and there is none to draw into".into(),
                    );
                };
                // SAFETY: the caller promises the window outlives the plugin's GUI.
                if let Err(e) = unsafe { gui.set_parent(&mut handle, parent.as_clap()) } {
                    gui.destroy(&mut handle);
                    return Err(format!("it would not draw into our window: {e}"));
                }
            }
        }

        if let Err(e) = gui.show(&mut handle) {
            gui.destroy(&mut handle);
            return Err(format!("its window would not come up: {e}"));
        }
        live.window = true;
        Ok(style)
    }

    /// The size an embedded plugin has asked its window to become, once, or `None` for a
    /// plugin that has not asked since we last looked.
    ///
    /// Asked every time round the main thread. Surge XT's zoom menu is this: the plugin asks,
    /// the app resizes the window it made and tells the plugin what it now is with
    /// [`resize_window`](Studio::resize_window).
    pub fn wanted_size(&mut self, track: u16) -> Option<WindowSize> {
        let live = self.at(track)?;
        if !live.window {
            return None;
        }
        live.instance
            .access_shared_handler(|shared| shared.take_wanted_size())
            .map(WindowSize::from_gui)
    }

    /// Tell the plugin how big the window it is drawing into now is, so it can lay itself out
    /// to fit. For an embedded window only; a floating one sizes itself.
    pub fn resize_window(&mut self, track: u16, size: WindowSize) {
        let Some(live) = self.at(track) else { return };
        if !live.window {
            return;
        }
        let Some(gui) = live
            .instance
            .plugin_shared_handle()
            .get_extension::<PluginGui>()
        else {
            return;
        };
        // Nothing to be done about a plugin that refuses the size its own window is, beyond
        // not pretending otherwise: what it draws is its business.
        let _ = gui.set_size(&mut live.instance.plugin_handle(), size.as_gui());
    }

    /// Shut it again.
    pub fn close_window(&mut self, track: u16) {
        if let Some(live) = self.at(track) {
            live.shut_window();
        }
    }

    /// Whether the plugin on this track has its window up.
    pub fn window_open(&self, track: u16) -> bool {
        self.live
            .get(track as usize)
            .and_then(|one| one.as_ref())
            .is_some_and(|live| live.window)
    }

    /// The main thread coming round, which is where a plugin gets everything it cannot do for
    /// itself: the callback it asked for, the timers it registered, and the acknowledgement
    /// that the window it just closed can go.
    ///
    /// Called from the same poll that drives the playhead, so about sixty times a second. That
    /// is also the granularity of the timers, which is enough for a plugin repainting itself
    /// and is all a plugin gets to assume — CLAP lets the host slow a timer down.
    pub fn tick(&mut self) {
        let now = Instant::now();
        for slot in self.live.iter_mut() {
            let Some(live) = slot.as_mut() else { continue };
            live.tick(now);
        }
    }

    fn at(&mut self, track: u16) -> Option<&mut Live> {
        self.live.get_mut(track as usize)?.as_mut()
    }
}

/// One loaded plugin, as a [`Studio`] holds it.
struct Live {
    instance: PluginInstance<Weetbeats>,
    /// Whether the plugin currently has its window up. Ours to remember: CLAP has no way of
    /// asking, and the plugin only tells us when the answer changes.
    window: bool,
}

/// A plugin that is going has to have its window taken down first: CLAP says a plugin still
/// holding a GUI has not finished with it. Here rather than at each of the places a plugin can
/// go — unloaded, replaced, or the whole studio closing — because a window left open through
/// one of them is a plugin destroyed mid-repaint.
impl Drop for Live {
    fn drop(&mut self) {
        self.shut_window();
    }
}

impl Live {
    /// Take the plugin's window down, if it has one up.
    fn shut_window(&mut self) {
        if !self.window {
            return;
        }
        self.window = false;
        let Some(gui) = self
            .instance
            .plugin_shared_handle()
            .get_extension::<PluginGui>()
        else {
            return;
        };
        let mut handle = self.instance.plugin_handle();
        let _ = gui.hide(&mut handle);
        gui.destroy(&mut handle);
    }

    fn tick(&mut self, now: Instant) {
        // The callback it asked for. Surge XT wants one while it loads a patch.
        let wants = self
            .instance
            .access_shared_handler(|shared| shared.wants_callback.swap(false, Ordering::Relaxed));
        if wants {
            self.instance.call_on_main_thread_callback();
        }

        // Its window has gone: the user clicked the close box on the plugin's own window
        // rather than ours. `Destroyed` means it has already taken the window apart and is
        // waiting for us to say we noticed.
        match self
            .instance
            .access_shared_handler(|shared| shared.take_window_news())
        {
            Some(WindowNews::Destroyed) => {
                self.window = false;
                if let Some(gui) = self
                    .instance
                    .plugin_shared_handle()
                    .get_extension::<PluginGui>()
                {
                    gui.destroy(&mut self.instance.plugin_handle());
                }
            }
            Some(WindowNews::Closed) => self.shut_window(),
            None => {}
        }

        // And its timers. A plugin with a window almost always has one: it is how a GUI made
        // of somebody else's widgets gets to repaint.
        let mut due = [TimerId(0); MAX_TIMERS];
        let count = self
            .instance
            .access_handler_mut(|main| main.timers_due(now, &mut due));
        if count == 0 {
            return;
        }
        let Some(timers) = self
            .instance
            .plugin_shared_handle()
            .get_extension::<PluginTimer>()
        else {
            return;
        };
        let mut handle = self.instance.plugin_handle();
        for id in &due[..count] {
            timers.on_timer(&mut handle, *id);
        }
    }
}

/// What the app thread asks the plugin thread to do.
enum Job {
    Scan(Sender<Vec<Found>>),
    Load {
        track: u16,
        path: PathBuf,
        id: String,
        /// The plugin's own saved settings, from the project folder.
        state: Option<Vec<u8>>,
        reply: Sender<Result<Loaded, String>>,
    },
    Unload(u16),
    Params {
        track: u16,
        reply: Sender<Vec<Param>>,
    },
    SaveState {
        track: u16,
        reply: Sender<Option<Vec<u8>>>,
    },
    /// A slot the audio thread has finished with, come home to be dropped.
    Bin(Box<Slot>),
    /// Give any plugin that asked for it its trip round the main thread.
    Tick,
}

/// A [`Studio`] on a thread of its own, for anything that is never going to open a window.
///
/// The tests use one, because a plugin instance has to stay on one thread and a test is not
/// running on the app's. The app does not: a window has to be made on the process's first
/// thread, so the app keeps its studio there and calls it directly.
///
/// Every method blocks until the thread answers, which is what makes it safe to call from
/// anywhere.
pub struct Desk {
    jobs: Sender<Job>,
    sample_rate: f64,
}

impl Desk {
    /// Start the plugin thread. Its studio lives and dies with it.
    pub fn start(sample_rate: f64) -> Desk {
        let (jobs, inbox) = channel();
        std::thread::Builder::new()
            .name("weetbeats-plugins".into())
            .spawn(move || run(inbox, sample_rate))
            .expect("could not start the plugin thread");
        Desk { jobs, sample_rate }
    }

    pub fn sample_rate(&self) -> f64 {
        self.sample_rate
    }

    pub fn scan(&self) -> Vec<Found> {
        let (reply, answer) = channel();
        if self.jobs.send(Job::Scan(reply)).is_err() {
            return Vec::new();
        }
        answer.recv().unwrap_or_default()
    }

    pub fn load(
        &self,
        track: u16,
        path: &Path,
        id: &str,
        state: Option<Vec<u8>>,
    ) -> Result<Loaded, String> {
        let (reply, answer) = channel();
        self.jobs
            .send(Job::Load {
                track,
                path: path.to_path_buf(),
                id: id.to_string(),
                state,
                reply,
            })
            .map_err(|_| "the plugin thread has stopped".to_string())?;
        answer
            .recv()
            .map_err(|_| "the plugin thread gave up on that one".to_string())?
    }

    pub fn unload(&self, track: u16) {
        let _ = self.jobs.send(Job::Unload(track));
    }

    pub fn bin(&self, slot: Box<Slot>) {
        let _ = self.jobs.send(Job::Bin(slot));
    }

    pub fn params(&self, track: u16) -> Vec<Param> {
        let (reply, answer) = channel();
        if self.jobs.send(Job::Params { track, reply }).is_err() {
            return Vec::new();
        }
        answer.recv().unwrap_or_default()
    }

    pub fn save_state(&self, track: u16) -> Option<Vec<u8>> {
        let (reply, answer) = channel();
        self.jobs.send(Job::SaveState { track, reply }).ok()?;
        answer.recv().ok().flatten()
    }

    pub fn tick(&self) {
        let _ = self.jobs.send(Job::Tick);
    }
}

fn run(inbox: Receiver<Job>, sample_rate: f64) {
    let mut studio = Studio::new(sample_rate);
    while let Ok(job) = inbox.recv() {
        match job {
            Job::Scan(reply) => {
                let _ = reply.send(studio.scan());
            }
            Job::Load {
                track,
                path,
                id,
                state,
                reply,
            } => {
                let _ = reply.send(studio.load(track, &path, &id, state));
            }
            Job::Unload(track) => studio.unload(track),
            Job::Params { track, reply } => {
                let _ = reply.send(studio.params(track));
            }
            Job::SaveState { track, reply } => {
                let _ = reply.send(studio.save_state(track));
            }
            Job::Bin(slot) => studio.bin(slot),
            Job::Tick => studio.tick(),
        }
    }
}

/// Open every `.clap` we can find and ask what is inside it.
fn scan_all(entries: &mut Vec<(PathBuf, PluginEntry)>) -> Vec<Found> {
    let mut files = Vec::new();
    for root in search_paths() {
        clap_files(&root, 0, &mut files);
    }
    files.sort();
    files.dedup();

    let mut found = Vec::new();
    for file in files {
        let Some(entry) = entry_for(entries, &file) else {
            continue;
        };
        let Some(factory) = entry.get_plugin_factory() else {
            continue;
        };
        for descriptor in factory.plugin_descriptors() {
            let Some(id) = descriptor.id() else { continue };
            let text = |value: Option<&std::ffi::CStr>| {
                value
                    .map(|v| v.to_string_lossy().into_owned())
                    .unwrap_or_default()
            };
            // Features are a list of tags; "instrument" is the one that says notes go in.
            let instrument = descriptor
                .features()
                .any(|feature| feature.to_bytes() == b"instrument");
            found.push(Found {
                path: file.to_string_lossy().into_owned(),
                id: id.to_string_lossy().into_owned(),
                name: text(descriptor.name()),
                vendor: text(descriptor.vendor()),
                instrument,
            });
        }
    }
    found.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    found
}

/// The entry for a file, loading it if this is the first time we have looked inside it.
///
/// Loading a `.clap` runs code somebody else wrote, which is what makes this unsafe and what
/// makes a plugin able to take the app down with it. There is no safe version: hosting is
/// running other people's code.
fn entry_for<'a>(
    entries: &'a mut Vec<(PathBuf, PluginEntry)>,
    file: &Path,
) -> Option<&'a PluginEntry> {
    if let Some(at) = entries.iter().position(|(path, _)| path == file) {
        return Some(&entries[at].1);
    }
    // SAFETY: nothing here can make loading a third party library safe. The rest of the
    // module keeps to CLAP's thread rules, which is the part we can be held to.
    let entry = unsafe { PluginEntry::load(file) }.ok()?;
    entries.push((file.to_path_buf(), entry));
    entries.last().map(|(_, entry)| entry)
}

#[allow(clippy::too_many_arguments)]
fn load_one(
    entries: &mut Vec<(PathBuf, PluginEntry)>,
    live: &mut [Option<Live>],
    track: u16,
    path: &Path,
    id: &str,
    state: Option<Vec<u8>>,
    sample_rate: f64,
) -> Result<Loaded, String> {
    if track as usize >= live.len() {
        return Err("that is not a track".into());
    }
    let name_of = |path: &Path| {
        path.file_stem()
            .and_then(|n| n.to_str())
            .unwrap_or("that plugin")
            .to_string()
    };
    let entry =
        entry_for(entries, path).ok_or_else(|| format!("{} could not be opened", name_of(path)))?;
    let factory = entry
        .get_plugin_factory()
        .ok_or_else(|| format!("{} holds no plugins", name_of(path)))?;

    let wanted = CString::new(id).map_err(|_| "that is not a plugin id".to_string())?;
    let descriptor = factory
        .plugin_descriptors()
        .find(|d| d.id().is_some_and(|found| found == wanted.as_c_str()))
        .ok_or_else(|| format!("{} is not in {}", id, name_of(path)))?;
    let name = descriptor
        .name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| name_of(path));

    let info = HostInfo::new(
        "Weetbeats",
        "Weetbeats",
        "https://github.com/nathansherburn/weetbeats",
        env!("CARGO_PKG_VERSION"),
    )
    .map_err(|e| format!("could not introduce ourselves to {name}: {e}"))?;

    let mut instance = PluginInstance::<Weetbeats>::new(
        |_| Shared::default(),
        |_| MainThread::default(),
        entry,
        wanted.as_c_str(),
        &info,
    )
    .map_err(|e| format!("{name} would not start: {e}"))?;

    // Its own settings, before it is activated: a plugin is allowed to want a different
    // buffer layout once it has read them.
    if let Some(blob) = state {
        if let Some(extension) = instance
            .plugin_shared_handle()
            .get_extension::<PluginState>()
        {
            let mut reader = blob.as_slice();
            let _ = extension.load(&mut instance.plugin_handle(), &mut reader);
        }
    }

    // Before activating, because a shape we cannot provide is a reason not to start the
    // plugin at all rather than something to find out in the middle of a block.
    let ports = read_ports(&mut instance);
    if let Err(why) = ports.manageable() {
        return Err(format!("{name} cannot be played here: {why}"));
    }

    let processor = instance
        .activate(
            |_, _| (),
            PluginAudioConfiguration {
                sample_rate,
                // The engine chops every callback at step boundaries, so a block can be one
                // frame long. The ceiling is the engine's own.
                min_frames_count: 1,
                max_frames_count: MAX_BLOCK as u32,
            },
        )
        .map_err(|e| format!("{name} would not start processing: {e}"))?;

    let params = read_params(&mut instance);
    live[track as usize] = Some(Live {
        instance,
        window: false,
    });

    Ok(Loaded {
        slot: Slot::new(processor.into(), &ports),
        name,
        params,
    })
}

/// The ports the plugin says it has, each way, and how many channels each of them carries.
///
/// A host has to hand a plugin exactly the ports it declared. A plugin with no `audio-ports`
/// extension at all declares none, and gets none: it is telling us it does not do audio.
fn read_ports(instance: &mut PluginInstance<Weetbeats>) -> Ports {
    let Some(extension) = instance
        .plugin_shared_handle()
        .get_extension::<PluginAudioPorts>()
    else {
        return Ports::default();
    };
    let mut handle = instance.plugin_handle();
    let mut buffer = AudioPortInfoBuffer::new();
    // A plain loop rather than an iterator: the info borrows the buffer it was written into,
    // so it has to be read and finished with before the next port is asked about.
    let mut side = |is_input: bool| {
        let count = extension.count(&mut handle, is_input);
        let mut channels = Vec::with_capacity(count as usize);
        for index in 0..count {
            let found = extension
                .get(&mut handle, index, is_input, &mut buffer)
                .map(|port| port.channel_count as usize);
            if let Some(found) = found {
                channels.push(found);
            }
        }
        channels
    };
    Ports {
        inputs: side(true),
        outputs: side(false),
    }
}

/// Everything the plugin says about its parameters, and where they are now.
fn read_params(instance: &mut PluginInstance<Weetbeats>) -> Vec<Param> {
    let Some(extension) = instance
        .plugin_shared_handle()
        .get_extension::<PluginParams>()
    else {
        return Vec::new();
    };
    let mut handle = instance.plugin_handle();
    let count = extension.count(&mut handle);
    let mut params = Vec::with_capacity(count as usize);
    let mut buffer = ParamInfoBuffer::new();
    let mut text = [0u8; 256];
    for index in 0..count {
        let Some(info) = extension.get_info(&mut handle, index, &mut buffer) else {
            continue;
        };
        if info.flags.contains(ParamInfoFlags::IS_HIDDEN) {
            continue;
        }
        let id = info.id;
        let name = String::from_utf8_lossy(info.name).into_owned();
        let module = String::from_utf8_lossy(info.module).into_owned();
        let (min, max, stepped) = (
            info.min_value,
            info.max_value,
            info.flags.contains(ParamInfoFlags::IS_STEPPED),
        );
        let value = extension
            .get_value(&mut handle, id)
            .unwrap_or(info.default_value);
        let said = extension
            .value_to_text(&mut handle, id, value, &mut text)
            .ok()
            .map(|said| String::from_utf8_lossy(said).into_owned())
            .unwrap_or_default();
        params.push(Param {
            id: id.into(),
            name,
            module,
            min,
            max,
            value,
            text: said,
            stepped,
        });
    }
    params
}

fn save_state(instance: &mut PluginInstance<Weetbeats>) -> Option<Vec<u8>> {
    let extension = instance
        .plugin_shared_handle()
        .get_extension::<PluginState>()?;
    let mut blob = Vec::new();
    extension
        .save(&mut instance.plugin_handle(), &mut blob)
        .ok()?;
    Some(blob)
}

// --- the audio thread's half -------------------------------------------------

/// One note the plugin is holding, so it can be let go of when it ends.
#[derive(Clone, Copy)]
struct Ringing {
    key: u16,
    note_id: u32,
    /// Steps until the note is over. Counted down a step at a time, because a note's length
    /// is in steps and the engine chops its blocks at step boundaries: a note off is always
    /// at the very start of a block, which is exactly where it belongs.
    steps_left: u32,
    /// Which pattern started it, so a pattern going quiet can let go of its own notes and
    /// leave everybody else's ringing.
    pattern: u16,
}

/// How many ports a plugin wants, and how many channels each of them has. Read from the
/// plugin once, when it is loaded, because the host has to hand it exactly the ports it
/// declared: one too few is a crash waiting to happen.
#[derive(Clone, Debug, Default)]
pub struct Ports {
    pub inputs: Vec<usize>,
    pub outputs: Vec<usize>,
}

impl Ports {
    /// Whether this is a shape we can honestly provide. Being wrong here is not a wrong
    /// sound, it is a plugin writing into memory we did not give it.
    fn manageable(&self) -> Result<(), String> {
        for (side, ports) in [("takes", &self.inputs), ("makes", &self.outputs)] {
            if ports.len() > MAX_PORTS {
                return Err(format!(
                    "it {side} {} lots of audio at once, which is more than we can hand it",
                    ports.len()
                ));
            }
            if let Some(wide) = ports.iter().find(|channels| **channels > MAX_PORT_CHANNELS) {
                return Err(format!(
                    "it {side} audio {wide} channels wide, which is more than we can hand it"
                ));
            }
        }
        Ok(())
    }
}

/// A plugin as the audio thread has it: the processor, the buffers it reads and writes, and
/// the notes going into it.
///
/// Every field is allocated here, on the app thread, and only ever indexed after that. The
/// whole thing goes across to the audio thread in one box.
pub struct Slot {
    processor: Processor,
    /// Silence going in, a buffer per channel per port. An instrument does not read it, but
    /// it declared the ports and CLAP says it gets what it declared.
    ins: Vec<Vec<Vec<f32>>>,
    /// And what comes out. Its first port is the one we listen to.
    outs: Vec<Vec<Vec<f32>>>,
    ports_in: AudioPorts,
    ports_out: AudioPorts,
    events_in: EventBuffer,
    events_out: EventBuffer,
    ringing: [Option<Ringing>; MAX_PLUGIN_NOTES],
    next_note_id: u32,
    /// Where the track's level is heading and where it is now, so a change slides rather
    /// than steps.
    pub level: f32,
    level_now: f32,
}

/// Enough to name it in a command's debug output. Everything inside a slot is either a
/// plugin's private state or a few kilobytes of buffer, and neither is worth printing.
impl std::fmt::Debug for Slot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Slot")
    }
}

impl Slot {
    /// Build everything the audio thread will need, here on the app thread.
    ///
    /// A plugin with no output port at all still gets one buffer to write nothing into, so
    /// the render loop has somewhere to read from without checking every block.
    fn new(processor: Processor, ports: &Ports) -> Box<Slot> {
        // Exactly what the plugin declared, port for port and channel for channel. The
        // shape has already been checked as one we can provide.
        let room = |counts: &[usize]| -> Vec<Vec<Vec<f32>>> {
            counts
                .iter()
                .map(|channels| (0..*channels).map(|_| vec![0.0; MAX_BLOCK]).collect())
                .collect()
        };
        let ins = room(&ports.inputs);
        let outs = room(&ports.outputs);
        let total = |bufs: &[Vec<Vec<f32>>]| bufs.iter().map(|port| port.len()).sum::<usize>();
        Box::new(Slot {
            processor,
            ports_in: AudioPorts::with_capacity(total(&ins).max(1), ins.len().max(1)),
            ports_out: AudioPorts::with_capacity(total(&outs).max(1), outs.len().max(1)),
            ins,
            outs,
            events_in: EventBuffer::with_capacity(EVENT_CAPACITY),
            events_out: EventBuffer::with_capacity(EVENT_CAPACITY),
            ringing: [None; MAX_PLUGIN_NOTES],
            next_note_id: 1,
            level: 1.0,
            level_now: 1.0,
        })
    }

    /// Start a note, at the very start of the next block.
    ///
    /// `velocity` is where the pattern's fader has already been applied: a plugin makes one
    /// sound for the whole track, so there is nothing on the way out to put a per-pattern
    /// fader on. See the module docs.
    pub fn note_on(&mut self, pattern: u16, key: u8, velocity: f32, steps: u32) {
        let note_id = self.next_note_id;
        self.next_note_id = self.next_note_id.wrapping_add(1).max(1);
        let key = key as u16;

        let free = match self.ringing.iter().position(|one| one.is_none()) {
            Some(at) => at,
            None => {
                // Every slot is holding a note. Let the first one go rather than losing the
                // new one: a stuck note is worse than a short one.
                self.let_go(0);
                0
            }
        };
        self.ringing[free] = Some(Ringing {
            key,
            note_id,
            steps_left: steps.max(1),
            pattern,
        });
        self.events_in.push(&NoteOnEvent::new(
            0,
            Pckn::new(0u16, 0u16, key, note_id),
            velocity.clamp(0.0, 1.0) as f64,
        ));
    }

    /// A step has gone by: everything that ends now, ends now.
    pub fn step(&mut self) {
        for at in 0..self.ringing.len() {
            let Some(note) = self.ringing[at] else {
                continue;
            };
            if note.steps_left <= 1 {
                self.let_go(at);
            } else {
                self.ringing[at] = Some(Ringing {
                    steps_left: note.steps_left - 1,
                    ..note
                });
            }
        }
    }

    /// Let go of everything one pattern started. For a pattern that has been muted, or has
    /// stopped playing part way through a note.
    pub fn release_pattern(&mut self, pattern: u16) {
        for at in 0..self.ringing.len() {
            if self.ringing[at].is_some_and(|one| one.pattern == pattern) {
                self.let_go(at);
            }
        }
    }

    /// Let go of everything. For stopping, and for the panic button.
    pub fn release_all(&mut self) {
        for at in 0..self.ringing.len() {
            self.let_go(at);
        }
    }

    fn let_go(&mut self, at: usize) {
        let Some(note) = self.ringing[at].take() else {
            return;
        };
        self.events_in.push(&NoteOffEvent::new(
            0,
            Pckn::new(0u16, 0u16, note.key, note.note_id),
            0.0,
        ));
    }

    /// Move a parameter, in the one way CLAP has of moving one: an event into the next block.
    pub fn set_param(&mut self, param: u32, value: f64) {
        let Some(id) = ClapId::from_raw(param) else {
            return;
        };
        self.events_in.push(&ParamValueEvent::new(
            0,
            id,
            Pckn::new(Match::All, Match::All, Match::All, Match::All),
            value,
            Cookie::empty(),
        ));
    }

    /// Run the plugin for one block and add what comes out into `mix`, interleaved stereo.
    ///
    /// Hot path: no allocation. The event buffers are cleared rather than dropped, the audio
    /// port wrappers reuse the lists they were given capacity for, and nothing here grows.
    pub fn render(&mut self, mix: &mut [f32], frames: usize, gain_inc: f32) {
        let frames = frames.min(MAX_BLOCK);
        let target = self.level;
        let start = self.level_now;
        let end =
            start + (target - start).clamp(-gain_inc * frames as f32, gain_inc * frames as f32);
        self.level_now = end;
        let step = (end - start) / frames.max(1) as f32;

        let Ok(started) = self.processor.ensure_processing_started() else {
            self.events_in.clear();
            return;
        };

        // Exactly the ports the plugin declared, no more and no fewer. The wrappers reuse the
        // lists `AudioPorts` was given capacity for, so none of this allocates.
        let input = self
            .ports_in
            .with_input_buffers(self.ins.iter_mut().map(|port| {
                AudioPortBuffer {
                    latency: 0,
                    channels: AudioPortBufferType::f32_input_only(
                        port.iter_mut()
                            .map(|channel| InputChannel::constant(&mut channel[..frames])),
                    ),
                }
            }));
        let mut output = self
            .ports_out
            .with_output_buffers(self.outs.iter_mut().map(|port| AudioPortBuffer {
                latency: 0,
                channels: AudioPortBufferType::f32_output_only(
                    port.iter_mut().map(|channel| &mut channel[..frames]),
                ),
            }));

        self.events_out.clear();
        let events_in = InputEvents::from_buffer(&self.events_in);
        let mut events_out = OutputEvents::from_buffer(&mut self.events_out);
        let ran = started.process(&input, &mut output, &events_in, &mut events_out, None, None);
        self.events_in.clear();
        if ran.is_err() {
            return;
        }

        // The plugin's first output port is the one we listen to; a mono one is heard from
        // both speakers and anything wider than stereo has the rest left alone. A plugin
        // with no output at all — or a port with no channels — makes no sound, which is
        // silly of it but not our business to argue with.
        let Some(left) = self.outs.first().and_then(|port| port.first()) else {
            return;
        };
        let right = self.outs[0].get(1).unwrap_or(left);
        for frame in 0..frames {
            let level = start + step * frame as f32;
            mix[frame * 2] += left[frame] * level;
            mix[frame * 2 + 1] += right[frame] * level;
        }
    }

    /// Put the level where it is going without sliding, for a change made while stopped.
    pub fn settle(&mut self) {
        self.level_now = self.level;
    }
}
