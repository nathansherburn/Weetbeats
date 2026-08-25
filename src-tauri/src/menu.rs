//! The menu bar.
//!
//! Opening and saving live here rather than as buttons in the window: they are the two
//! things every Mac app keeps in the same place, and a music app has better uses for the
//! space along the top.
//!
//! The whole Edit menu is ours, and it has to be: on macOS a menu item's key equivalent is
//! handled before the window ever sees the key, so the standard items would swallow cmd-Z,
//! cmd-C, cmd-V and cmd-A and hand them to the webview — which would undo typing in a text
//! field and nothing else, and would mean the piano roll could never have them. Every item
//! here emits an event instead, and the window decides what it means: the notes or blocks
//! picked out, or the text field with the focus.
//!
//! Everything else is Tauri's standard menu.

use tauri::menu::{Menu, MenuEvent, MenuItemBuilder, SubmenuBuilder};
use tauri::AppHandle;

use crate::commands;

/// Build the menu bar and hand it to the app.
pub fn install(app: &AppHandle) -> tauri::Result<()> {
    let open = MenuItemBuilder::with_id("open", "Open…")
        .accelerator("CmdOrCtrl+O")
        .build(app)?;
    let save = MenuItemBuilder::with_id("save", "Save")
        .accelerator("CmdOrCtrl+S")
        .build(app)?;
    let save_as = MenuItemBuilder::with_id("save_as", "Save As…")
        .accelerator("Shift+CmdOrCtrl+S")
        .build(app)?;

    let file = SubmenuBuilder::new(app, "File")
        .item(&open)
        .separator()
        .item(&save)
        .item(&save_as)
        .separator()
        .close_window()
        .build()?;

    let undo = MenuItemBuilder::with_id("undo", "Undo")
        .accelerator("CmdOrCtrl+Z")
        .build(app)?;
    let redo = MenuItemBuilder::with_id("redo", "Redo")
        .accelerator("Shift+CmdOrCtrl+Z")
        .build(app)?;

    // Ours rather than the standard cut, copy, paste and select all, so that the keys reach
    // the window. What each one does depends on what has the focus, which is the window's
    // business: the notes or blocks picked out, or the text field being typed in.
    let cut = MenuItemBuilder::with_id("cut", "Cut")
        .accelerator("CmdOrCtrl+X")
        .build(app)?;
    let copy = MenuItemBuilder::with_id("copy", "Copy")
        .accelerator("CmdOrCtrl+C")
        .build(app)?;
    let paste = MenuItemBuilder::with_id("paste", "Paste")
        .accelerator("CmdOrCtrl+V")
        .build(app)?;
    let duplicate = MenuItemBuilder::with_id("duplicate", "Duplicate")
        .accelerator("CmdOrCtrl+D")
        .build(app)?;
    let select_all = MenuItemBuilder::with_id("select_all", "Select All")
        .accelerator("CmdOrCtrl+A")
        .build(app)?;

    let edit = SubmenuBuilder::new(app, "Edit")
        .item(&undo)
        .item(&redo)
        .separator()
        .item(&cut)
        .item(&copy)
        .item(&paste)
        .item(&duplicate)
        .separator()
        .item(&select_all)
        .build()?;

    // Start from the standard menu and put ours where its File and Edit submenus were, so
    // there is one of each rather than two. On macOS the application submenu comes first.
    let menu = Menu::default(app)?;
    let position = if cfg!(target_os = "macos") { 1 } else { 0 };
    menu.remove_at(position + 1)?;
    menu.remove_at(position)?;
    menu.insert(&file, position)?;
    menu.insert(&edit, position + 1)?;
    app.set_menu(menu)?;
    Ok(())
}

/// Menu events arrive on the main thread, and a file dialog blocks until it is answered, so
/// nothing here is done here.
pub fn handle(app: &AppHandle, event: MenuEvent) {
    let app = app.clone();
    let id = event.id().0.clone();
    tauri::async_runtime::spawn_blocking(move || match id.as_str() {
        "open" => commands::open(&app),
        "save" => commands::save(&app),
        "save_as" => commands::save_as(&app),
        // The window does the stepping, because it has to redraw either way.
        "undo" => commands::stepped_by_menu(&app, true),
        "redo" => commands::stepped_by_menu(&app, false),
        // And the window does the editing, because what these mean depends on what has the
        // focus and it is the only one that knows.
        "cut" | "copy" | "paste" | "duplicate" | "select_all" => commands::edit_by_menu(&app, &id),
        // Everything else in the menu bar is Tauri's, and it handles its own.
        _ => {}
    });
}
