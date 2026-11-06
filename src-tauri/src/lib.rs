//! Tauri app crate: thin glue between the window and the engine.
//! All monitoring logic lives in the workspace crates under `crates/`.

mod commands;
mod dashboard;
mod events;
mod shutdown;
mod state;
mod tray;

use tauri::{Manager, WindowEvent};

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let app = tauri::Builder::default()
        // Must be registered first: a second launch focuses the existing window instead.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| tray::show_main(app)))
        .setup(|app| {
            state::init(app)?;
            tray::init(app)?;
            Ok(())
        })
        .on_window_event(|window, event| {
            // Close-to-tray: hide instead of closing; the engine keeps running.
            if let WindowEvent::CloseRequested { api, .. } = event
                && window.label() == tray::MAIN_WINDOW
            {
                api.prevent_close();
                let _ = window.hide();
                if let Some(state) = window.app_handle().try_state::<state::AppState>() {
                    state.engine.set_visible(false);
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_dashboard,
            commands::set_collector_enabled,
            commands::set_all_collectors_enabled,
            commands::refresh,
            commands::get_claude_folders,
            commands::add_claude_folder,
            commands::remove_claude_folder,
        ])
        .build(tauri::generate_context!());

    match app {
        Ok(app) => app.run(shutdown::on_run_event),
        Err(e) => {
            eprintln!("failed to start AgentPulse: {e}");
            std::process::exit(1);
        }
    }
}
