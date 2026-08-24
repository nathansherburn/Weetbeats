//! Keeping the plugin studio on the thread a window can be made on.
//!
//! ## Why this exists
//!
//! CLAP calls one thread the main thread and wants every plugin instance made, asked about
//! and destroyed on it. Which thread that is, is the host's choice — right up until a plugin
//! opens a window. A window is made of the platform's own widgets, and macOS will only make
//! those on the process's first thread. So the studio lives there: on Tauri's main thread,
//! the same one the webview draws on.
//!
//! That is a real cost and worth naming. Loading Surge XT takes a moment, and that moment is
//! now a moment the window is not repainting. The alternative — a thread of our own, which is
//! what [`Desk`](weetbeats_engine::plugins::Desk) still does for the tests — cannot open a
//! window at all.
//!
//! ## How
//!
//! The studio sits in a thread local that only the main thread ever touches, and every call
//! goes through [`MainDesk::with`], which either runs the job inline (already on the main
//! thread) or posts it there and waits. Callers cannot tell which, so a Tauri command can ask
//! for anything from wherever it happens to be running.

use std::cell::RefCell;
use std::path::Path;
use std::sync::mpsc::channel;
use std::thread::ThreadId;

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use tauri::{AppHandle, Manager};
use weetbeats_engine::plugins::{Found, Loaded, Param, ParentWindow, Slot, Studio};

thread_local! {
    /// The one studio, on the one thread. Made the first time anything asks for it, which is
    /// the first time the app touches a plugin at all.
    static STUDIO: RefCell<Option<Studio>> = const { RefCell::new(None) };
}

/// The studio, reachable from anywhere.
pub struct MainDesk {
    app: AppHandle,
    /// Which thread the studio is on, so a call already there does not post to itself and
    /// wait forever.
    main: ThreadId,
    sample_rate: f64,
}

impl MainDesk {
    /// Take note of the main thread. **Call this from it** — Tauri's `setup` is the place —
    /// because this is where the thread is decided.
    pub fn new(app: AppHandle, sample_rate: f64) -> MainDesk {
        MainDesk {
            app,
            main: std::thread::current().id(),
            sample_rate,
        }
    }

    /// Do something with the studio, wherever the caller happens to be.
    ///
    /// `None` when the main thread could not be reached, or when the studio is already in
    /// use further up the same stack. Neither should happen; both are reported rather than
    /// panicked over, because the one thing worse than a plugin not loading is the app going
    /// down with it.
    fn with<R, F>(&self, job: F) -> Option<R>
    where
        R: Send + 'static,
        F: FnOnce(&mut Studio) -> R + Send + 'static,
    {
        let rate = self.sample_rate;
        let run = move || {
            STUDIO.with(|cell| {
                let mut held = cell.try_borrow_mut().ok()?;
                let studio = held.get_or_insert_with(|| Studio::new(rate));
                Some(job(studio))
            })
        };
        if std::thread::current().id() == self.main {
            return run();
        }
        let (tx, rx) = channel();
        self.app
            .run_on_main_thread(move || {
                let _ = tx.send(run());
            })
            .ok()?;
        rx.recv().ok().flatten()
    }

    pub fn scan(&self) -> Vec<Found> {
        self.with(|studio| studio.scan()).unwrap_or_default()
    }

    /// Put a plugin on a track. Runs on the main thread, so a big synth is a pause.
    pub fn load(
        &self,
        track: u16,
        path: &Path,
        id: &str,
        state: Option<Vec<u8>>,
    ) -> Result<Loaded, String> {
        let path = path.to_path_buf();
        let id = id.to_string();
        self.with(move |studio| studio.load(track, &path, &id, state))
            .unwrap_or_else(|| Err("the plugin desk could not be reached".into()))
    }

    pub fn unload(&self, track: u16) {
        self.with(move |studio| studio.unload(track));
    }

    pub fn params(&self, track: u16) -> Vec<Param> {
        self.with(move |studio| studio.params(track))
            .unwrap_or_default()
    }

    pub fn save_state(&self, track: u16) -> Option<Vec<u8>> {
        self.with(move |studio| studio.save_state(track)).flatten()
    }

    /// A slot the audio thread has finished with, dropped where it was made.
    pub fn bin(&self, slot: Box<Slot>) {
        self.with(move |studio| studio.bin(slot));
    }

    pub fn tick(&self) {
        self.with(|studio| studio.tick());
    }

    /// Open the plugin's own window, floating above ours.
    pub fn open_window(&self, track: u16, title: String) -> Result<(), String> {
        let app = self.app.clone();
        self.with(move |studio| {
            // Asked for here rather than passed in: on macOS a window will only answer
            // questions about itself on the main thread, which is where this is running.
            let parent = our_window(&app);
            // SAFETY: the parent is the app's own main window. It is made before any plugin
            // can be loaded and it goes when the app does, so it outlives every plugin GUI.
            unsafe { studio.open_window(track, parent, &title) }
        })
        .unwrap_or_else(|| Err("the plugin desk could not be reached".into()))
    }

    pub fn close_window(&self, track: u16) {
        self.with(move |studio| studio.close_window(track));
    }

    pub fn window_open(&self, track: u16) -> bool {
        self.with(move |studio| studio.window_open(track))
            .unwrap_or(false)
    }
}

/// Our own window, in whatever terms the platform's windowing API uses.
///
/// A plugin's floating window is told to stay above this one. Wayland is the gap: CLAP has a
/// name for it but no way to name a particular window, so a plugin there gets a window that
/// floats above nothing in particular, which is a great deal better than no window.
fn our_window(app: &AppHandle) -> Option<ParentWindow> {
    let window = app.get_webview_window("main")?;
    let handle = window.window_handle().ok()?;
    match handle.as_raw() {
        RawWindowHandle::AppKit(mac) => Some(ParentWindow::Cocoa(mac.ns_view.as_ptr())),
        RawWindowHandle::Win32(windows) => Some(ParentWindow::Win32(
            windows.hwnd.get() as *mut std::ffi::c_void
        )),
        RawWindowHandle::Xlib(x11) => Some(ParentWindow::X11(x11.window)),
        _ => None,
    }
}
