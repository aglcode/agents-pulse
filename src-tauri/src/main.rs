// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // Claude Code runs AgentPulse as its status line (quota tap): answer and exit,
    // without opening a window.
    let args: Vec<String> = std::env::args().collect();
    if let Some(code) = agents_pulse_lib::run_statusline_tap(&args) {
        std::process::exit(code);
    }
    agents_pulse_lib::run()
}
