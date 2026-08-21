//! The menu bar.
//!
//! Opening and saving live here rather than as buttons in the window: they are the two
//! things every Mac app keeps in the same place, and a music app has better uses for the
//! space along the top.
//!
//! Everything else is Tauri's standard menu. The Edit submenu earns its keep even in an app
//! with almost no typing in it, because without it copy and paste stop working in the one
//! text field there is — renaming a pattern.

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

    // Start from the standard menu and put our File submenu where its one was, so there is
    // one File menu rather than two. On macOS the application submenu comes first.
    let menu = Menu::default(app)?;
    let position = if cfg!(target_os = "macos") { 1 } else { 0 };
    menu.remove_at(position)?;
    menu.insert(&file, position)?;
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
        // Everything else in the menu bar is Tauri's, and it handles its own.
        _ => {}
    });
}
