//! System tray: Show / Refresh / Quit. The window hides to the tray on close.

use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{App, AppHandle, Manager};

use crate::state::AppState;

pub const MAIN_WINDOW: &str = "main";

pub fn init(app: &mut App) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "show", "Show AgentPulse", true, None::<&str>)?;
    let refresh = MenuItem::with_id(app, "refresh", "Refresh now", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit AgentPulse", true, None::<&str>)?;
    let separator = PredefinedMenuItem::separator(app)?;
    let menu = Menu::with_items(app, &[&show, &refresh, &separator, &quit])?;

    let mut builder = TrayIconBuilder::with_id("agentpulse-tray")
        .tooltip("AgentPulse")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show" => show_main(app),
            "refresh" => {
                if let Some(state) = app.try_state::<AppState>() {
                    state.engine.refresh(None);
                }
            }
            // Goes through RunEvent::ExitRequested → graceful shutdown.
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                show_main(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder.build(app)?;
    Ok(())
}

pub fn show_main(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(MAIN_WINDOW) {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
    if let Some(state) = app.try_state::<AppState>() {
        state.engine.set_visible(true);
    }
    // Events were muted while hidden; tell the UI to refetch.
    crate::events::emit(app, crate::events::RESYNC, ());
}
