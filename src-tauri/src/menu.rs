//! The menu bar.
//!
//! Opening and saving live here rather than as buttons in the window: they are the two
//! things every Mac app keeps in the same place, and a music app has better uses for the
//! space along the top.
//!
//! Undo and redo are here too, and they have to be: on macOS a menu item's key equivalent
//! is handled before the window sees the key, so a standard Edit menu would swallow cmd-Z
//! and give it to the webview, which would undo typing in a text field and nothing else.
//! Ours emit an event instead, and the cut, copy, paste and select all items are the
//! standard ones, so the one text field in the app still works.
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

    let edit = SubmenuBuilder::new(app, "Edit")
        .item(&undo)
        .item(&redo)
        .separator()
        .cut()
        .copy()
        .paste()
        .select_all()
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
        // Everything else in the menu bar is Tauri's, and it handles its own.
        _ => {}
    });
}
