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
//!
//! ## Rooms
//!
//! Most plugins have a view and no window: Surge XT, and anything else built with JUCE, draws
//! into a window somebody else made. So this module makes them — a plain Tauri window with no
//! webview in it, one per track, kept in [`ROOMS`] beside the studio. Making it, sizing it to
//! what the plugin asks for, resizing it when the plugin's own zoom control asks again, and
//! taking it down when either side closes it, is what a host has to do that a floating window
//! did for itself.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;
use std::sync::mpsc::channel;
use std::thread::ThreadId;

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use tauri::{AppHandle, LogicalSize, Manager, PhysicalSize, Size, Window, WindowEvent};
use weetbeats_engine::plugins::{
    Found, Loaded, Param, ParentWindow, Slot, Studio, WindowSize, WindowStyle,
};

/// How big a room is before the plugin in it has said how big it should be. Never seen for
/// long: the window is built hidden and shown once the plugin has been asked.
const UNTIL_ASKED: f64 = 400.0;

thread_local! {
    /// The one studio, on the one thread. Made the first time anything asks for it, which is
    /// the first time the app touches a plugin at all.
    static STUDIO: RefCell<Option<Studio>> = const { RefCell::new(None) };
    /// The windows we have made for plugins that draw into ours, by track. Main thread only,
    /// like the studio, and for the same reason.
    static ROOMS: RefCell<HashMap<u16, Room>> = RefCell::new(HashMap::new());
}

/// A window we made for a plugin to draw into.
struct Room {
    window: Window,
    /// Whether the sizes the plugin quotes are logical pixels. CLAP's rule, and it differs by
    /// platform, so the plugin's side of it is remembered rather than worked out again here.
    logical: bool,
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
        self.with(move |studio| {
            // Whatever was on the track goes first, and its room with it: the next plugin may
            // not want a window at all, and an empty one left over is a puzzle. In that order,
            // because the plugin has to let go of the window before the window goes — which is
            // also why this is not left to the unload inside `load`.
            studio.unload(track);
            shut_room(track);
            studio.load(track, &path, &id, state)
        })
        .unwrap_or_else(|| Err("the plugin desk could not be reached".into()))
    }

    pub fn unload(&self, track: u16) {
        self.with(move |studio| {
            studio.unload(track);
            shut_room(track);
        });
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

    /// The main thread coming round, for the plugins and for the windows they are in.
    ///
    /// Both halves are here because both are the same trip: a plugin gets its callback and its
    /// timers, and then the rooms are looked over — resized to whatever the plugin asked for
    /// while we were away, and taken down if the plugin has let go of its window.
    pub fn tick(&self) {
        self.with(|studio| {
            studio.tick();
            for track in rooms() {
                if !studio.window_open(track) {
                    // The plugin closed its own GUI: it told us through the host's gui
                    // extension and the studio has already destroyed it. The window it was
                    // drawing into has nothing in it now.
                    shut_room(track);
                    continue;
                }
                if let Some(size) = studio.wanted_size(track) {
                    // Surge XT's zoom menu is this. Resize the window first and tell the
                    // plugin second, so what it lays itself out for is what it has got.
                    if fit(track, size) {
                        studio.resize_window(track, size);
                    }
                }
            }
        });
    }

    /// Open the plugin's window: its own floating above ours, or a room of ours for it to
    /// draw into, whichever the plugin will have.
    pub fn open_window(&self, track: u16, title: String) -> Result<(), String> {
        let app = self.app.clone();
        self.with(move |studio| open_now(&app, studio, track, &title))
            .unwrap_or_else(|| Err("the plugin desk could not be reached".into()))
    }

    pub fn close_window(&self, track: u16) {
        self.with(move |studio| {
            // The plugin lets go of the window before the window goes: CLAP says a plugin
            // still holding a GUI has not finished with it, and a view whose window has been
            // destroyed under it is a plugin drawing into nothing.
            studio.close_window(track);
            shut_room(track);
        });
    }

    pub fn window_open(&self, track: u16) -> bool {
        self.with(move |studio| studio.window_open(track))
            .unwrap_or(false)
    }
}

/// Open a plugin's window. **Main thread only** — everything here either makes a window or
/// asks one about itself, and neither can be done anywhere else.
fn open_now(app: &AppHandle, studio: &mut Studio, track: u16, title: &str) -> Result<(), String> {
    // Whatever went wrong making the room, kept here because all the plugin can be told is
    // that there is no window, and "no window" is not a reason.
    let mut trouble = None;
    // SAFETY: whichever window is handed over, it outlives the plugin's GUI. The app's own
    // window is made before any plugin can be loaded and goes when the app does; a room is
    // only ever taken down after the plugin has let go of the GUI drawn in it.
    let opened = unsafe {
        studio.open_window(track, title, |style, wanted| match style {
            // A floating window is the plugin's own; all it wants from us is something to
            // stay above, which is the app's window.
            WindowStyle::Floating => our_window(app),
            // An embedded one needs somewhere to be, made the size the plugin asked for and
            // hidden until there is something drawn in it.
            WindowStyle::Embedded { logical } => {
                match make_room(app, track, title, logical, wanted) {
                    Ok(parent) => Some(parent),
                    Err(e) => {
                        trouble = Some(e);
                        None
                    }
                }
            }
        })
    };
    if let Some(e) = trouble {
        shut_room(track);
        return Err(e);
    }
    match opened {
        // A room with nothing drawn in it is not a window anybody wants.
        Err(e) => {
            shut_room(track);
            Err(e)
        }
        Ok(WindowStyle::Embedded { .. }) => {
            show_room(track);
            Ok(())
        }
        Ok(WindowStyle::Floating) => Ok(()),
    }
}

/// Make a window for a plugin to draw into, and hand back what the platform calls it.
///
/// Made the size the plugin asked for, because a plugin lays its view out for the window it is
/// given: handing over the wrong size and resizing afterwards leaves the view where the old
/// size put it. Hidden until [`show_room`], so an empty window is never on screen.
///
/// Not resizable by dragging — a plugin has to be asked what sizes it can live with and told
/// what it has got, and until that is worth doing, the plugin's own zoom control is how its
/// window changes size.
fn make_room(
    app: &AppHandle,
    track: u16,
    title: &str,
    logical: bool,
    wanted: Option<WindowSize>,
) -> Result<ParentWindow, String> {
    shut_room(track);
    let mut builder = tauri::window::WindowBuilder::new(app, format!("plugin-{track}"))
        .title(title)
        .resizable(false)
        .visible(false);
    // Tauri's own default on macOS is a content view that runs the whole height of the
    // window, title bar included, for the sake of a webview there is none of here. A plugin's
    // view is put at the bottom left of whatever it is given, so in a window like that its top
    // inch ends up behind the title bar. This is the same window with the bar left out of the
    // content, which is what makes the size we asked for the size the plugin gets.
    #[cfg(target_os = "macos")]
    {
        builder = builder.title_bar_style(tauri::TitleBarStyle::Transparent);
    }
    builder = match wanted {
        // `inner_size` is logical, so a plugin quoting physical pixels has to be converted —
        // which needs a window to ask for a scale factor, so the app's own is asked.
        Some(size) => {
            let scale = match logical {
                true => 1.0,
                false => our_scale(app),
            };
            builder.inner_size(size.width as f64 / scale, size.height as f64 / scale)
        }
        None => builder.inner_size(UNTIL_ASKED, UNTIL_ASKED),
    };
    let window = builder
        .build()
        .map_err(|e| format!("we could not make a window for it to draw in: {e}"))?;

    // The close box on a window we made. The plugin has to let go of its GUI before the
    // window goes, so the close is refused and done properly a moment later, on the main
    // thread — which is where we may or may not be by the time this fires.
    let elsewhere = app.clone();
    window.on_window_event(move |event| {
        if let WindowEvent::CloseRequested { api, .. } = event {
            api.prevent_close();
            close_soon(&elsewhere, track);
        }
    });

    let parent = window_as_parent(&window)
        .ok_or_else(|| "this system does not name its windows in any way CLAP knows".to_string())?;
    ROOMS.with(|rooms| rooms.borrow_mut().insert(track, Room { window, logical }));
    Ok(parent)
}

/// Every track that has a room, so one can be worked on without the map held open.
fn rooms() -> Vec<u16> {
    ROOMS.with(|rooms| rooms.borrow().keys().copied().collect())
}

/// Make a room the size the plugin in it asked for. False if there is no such room.
fn fit(track: u16, size: WindowSize) -> bool {
    ROOMS.with(|rooms| {
        let rooms = rooms.borrow();
        let Some(room) = rooms.get(&track) else {
            return false;
        };
        let wanted = match room.logical {
            true => Size::Logical(LogicalSize::new(size.width as f64, size.height as f64)),
            false => Size::Physical(PhysicalSize::new(size.width, size.height)),
        };
        // A window that will not take the size is still a window, so this is not worth
        // failing the open over.
        let _ = room.window.set_size(wanted);
        true
    })
}

fn show_room(track: u16) {
    ROOMS.with(|rooms| {
        if let Some(room) = rooms.borrow().get(&track) {
            let _ = room.window.show();
            let _ = room.window.set_focus();
        }
    });
}

/// Take a plugin's room down, if it has one. Its plugin must already have let go of it.
fn shut_room(track: u16) {
    let room = ROOMS.with(|rooms| rooms.borrow_mut().remove(&track));
    if let Some(room) = room {
        let _ = room.window.destroy();
    }
}

/// Somebody clicked the close box on a room, so take the plugin's GUI down and then the room.
///
/// On the main thread, which is where both of those have to happen, and where this may or may
/// not already be. The studio is reached directly rather than through [`MainDesk::with`],
/// because by then this *is* on its thread.
///
/// The window is only destroyed once the plugin has actually let go of it. If the studio is
/// busy further up the same stack — a plugin loading, say — the job goes round again rather
/// than pulling a window out from under a view that is still drawing into it.
fn close_soon(app: &AppHandle, track: u16) {
    let again = app.clone();
    let _ = app.run_on_main_thread(move || {
        let closed = STUDIO.with(|cell| match cell.try_borrow_mut() {
            Ok(mut held) => match held.as_mut() {
                Some(studio) => {
                    studio.close_window(track);
                    true
                }
                // No studio at all means no plugin, so nothing is drawing in there.
                None => true,
            },
            Err(_) => false,
        });
        match closed {
            true => shut_room(track),
            false => close_soon(&again, track),
        }
    });
}

/// The app's own window, in whatever terms the platform's windowing API uses.
///
/// A plugin's floating window is told to stay above this one. Wayland is the gap: CLAP has a
/// name for it but no way to name a particular window, so a plugin there gets a window that
/// floats above nothing in particular, which is a great deal better than no window.
fn our_window(app: &AppHandle) -> Option<ParentWindow> {
    window_as_parent(&app.get_webview_window("main")?)
}

/// How many physical pixels the screen puts in a logical one, as the app's own window sees it.
/// One if there is no window to ask, which cannot happen while a plugin is loaded.
fn our_scale(app: &AppHandle) -> f64 {
    app.get_webview_window("main")
        .and_then(|window| window.scale_factor().ok())
        .unwrap_or(1.0)
}

/// Any of our windows, as the platform names it — a floating window's perch or an embedded
/// one's canvas, depending on which window was asked.
fn window_as_parent(window: &impl HasWindowHandle) -> Option<ParentWindow> {
    match window.window_handle().ok()?.as_raw() {
        RawWindowHandle::AppKit(mac) => Some(ParentWindow::Cocoa(mac.ns_view.as_ptr())),
        RawWindowHandle::Win32(windows) => Some(ParentWindow::Win32(
            windows.hwnd.get() as *mut std::ffi::c_void
        )),
        RawWindowHandle::Xlib(x11) => Some(ParentWindow::X11(x11.window)),
        _ => None,
    }
}
