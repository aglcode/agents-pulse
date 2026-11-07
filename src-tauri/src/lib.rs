//! Tauri app crate: thin glue between the window and the engine.
//! All monitoring logic lives in the workspace crates under `crates/`.

mod commands;
mod dashboard;
mod events;
mod shutdown;
mod state;
mod tray;

use tauri::{Manager, WindowEvent};

/// Must match `identifier` in `tauri.conf.json` (checked by a test): the quota tap
/// finds AgentPulse's data folder from it without starting Tauri.
pub const APP_IDENTIFIER: &str = "com.angelo-almonte.agents-pulse";

/// Quota tap mode: when Claude Code runs AgentPulse as its status line
/// (`agents-pulse --statusline-tap <folder key>`), handle that and return the exit
/// code, before Tauri, the window or the single-instance check start. `None` for a
/// normal launch.
pub fn run_statusline_tap(args: &[String]) -> Option<i32> {
    use agentpulse_collectors::claude_code::tap;
    let key = tap::tap_key(args)?;
    Some(match tap::tap_dir(APP_IDENTIFIER) {
        Some(dir) => tap::run(key, &dir, std::io::stdin().lock(), std::io::stdout().lock()),
        None => 0,
    })
}

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
            commands::preview_quota_tap,
            commands::set_quota_tap,
            commands::preview_forget_claude_folder,
            commands::forget_claude_folder,
            commands::preview_skip_default_claude_folder,
            commands::skip_default_claude_folder,
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

#[cfg(test)]
mod tests {
    #[test]
    fn app_identifier_matches_tauri_config() {
        let conf: serde_json::Value = serde_json::from_str(include_str!("../tauri.conf.json")).unwrap_or_default();
        assert_eq!(conf["identifier"].as_str(), Some(super::APP_IDENTIFIER));
    }
}
