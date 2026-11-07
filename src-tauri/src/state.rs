//! App state: logging, database, engine, event bridge.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
#[cfg(debug_assertions)]
use std::time::Duration;

use agentpulse_collectors::claude_code::ClaudeCodeCollector;
use agentpulse_collectors::codex::CodexCollector;
use agentpulse_collectors::gemini_cli::GeminiCliCollector;
use agentpulse_core::collector::Collector;
use agentpulse_engine::{Engine, EngineConfig};
use tauri::{App, Manager};
use tracing_appender::non_blocking::WorkerGuard;

/// `app_settings` key: JSON array of Claude Code config folders the user added.
pub const CLAUDE_FOLDERS_SETTING: &str = "claude_code.added_folders";

pub struct AppState {
    pub engine: Arc<Engine>,
    /// The Claude Code collector, kept so its folder list can be changed at runtime.
    pub claude: Option<Arc<ClaudeCodeCollector>>,
    /// The Gemini CLI collector, kept so the dashboard can say whether telemetry is on.
    pub gemini: Option<Arc<GeminiCliCollector>>,
    /// Flushes the log file on drop; taken during shutdown.
    pub log_guard: Mutex<Option<WorkerGuard>>,
    pub shutting_down: AtomicBool,
    pub shutdown_done: AtomicBool,
}

pub fn init(app: &mut App) -> anyhow::Result<()> {
    let log_guard = init_logging(app)?;

    let data_dir = app.path().app_data_dir()?;
    let db_path = data_dir.join("agentpulse.db");
    let (writer, reader) = agentpulse_store::open(&db_path)?;
    tracing::info!(schema = agentpulse_store::SCHEMA_VERSION, "database ready");

    let home = app.path().home_dir().ok();
    let config = EngineConfig { home_dir: home.clone(), ..EngineConfig::default() };
    // Reuse Tauri's Tokio runtime; the engine never creates its own.
    let rt = tauri::async_runtime::handle().inner().clone();
    // Opt-in: only collectors the user enabled earlier start now.
    let enabled = Engine::enabled_from_settings(&reader);
    tracing::info!(enabled = enabled.len(), "collectors enabled from settings");
    if app.config().identifier != crate::APP_IDENTIFIER {
        tracing::warn!("app identifier differs from APP_IDENTIFIER; the quota tap would write elsewhere");
    }
    // AgentPulse's own folder for quota tap snapshots. Created up front so the
    // collector can watch it from the start.
    let tap_dir = agentpulse_collectors::claude_code::tap::tap_dir(crate::APP_IDENTIFIER)
        .filter(|d| std::fs::create_dir_all(d).is_ok());
    let claude = home.as_deref().map(|h| {
        let c = ClaudeCodeCollector::new(
            h.to_path_buf(),
            ClaudeCodeCollector::env_config_dir(),
            added_claude_folders(&reader),
        );
        Arc::new(match tap_dir.clone() {
            Some(d) => c.with_tap_dir(d),
            None => c,
        })
    });
    let gemini = home.as_deref().map(|h| Arc::new(GeminiCliCollector::new(GeminiCliCollector::default_home(h))));
    let engine = Arc::new(Engine::start(
        &rt,
        collectors(home.as_deref(), claude.clone(), gemini.clone()),
        &enabled,
        writer,
        reader,
        config,
    ));

    crate::events::spawn_bridge(app.handle().clone(), engine.clone());

    app.manage(AppState {
        engine,
        claude,
        gemini,
        log_guard: Mutex::new(Some(log_guard)),
        shutting_down: AtomicBool::new(false),
        shutdown_done: AtomicBool::new(false),
    });
    Ok(())
}

/// Every registered collector. Registration doesn't start anything: collectors are
/// opt-in and only run once the user enables them. Debug builds also get a synthetic
/// collector to exercise the pipeline; release builds never do.
fn collectors(
    user_home: Option<&Path>,
    claude: Option<Arc<ClaudeCodeCollector>>,
    gemini: Option<Arc<GeminiCliCollector>>,
) -> Vec<Arc<dyn Collector>> {
    let mut list: Vec<Arc<dyn Collector>> = Vec::new();
    if let Some(c) = claude {
        list.push(c);
    }
    if let Some(home) = user_home {
        list.push(Arc::new(CodexCollector::new(CodexCollector::default_home(home))));
    }
    if let Some(g) = gemini {
        list.push(g);
    }
    if user_home.is_none() {
        tracing::warn!("home directory unknown; local collectors unavailable");
    }
    #[cfg(debug_assertions)]
    {
        use agentpulse_collectors::mock::{MockBehavior, MockCollector};
        list.push(Arc::new(MockCollector::new("mock-healthy", MockBehavior::Healthy, Duration::from_secs(15))));
    }
    list
}

/// User-added Claude Code folders from settings. Missing or malformed → none.
fn added_claude_folders(reader: &agentpulse_store::Reader) -> Vec<PathBuf> {
    reader
        .with_conn_blocking(|c| agentpulse_store::get_setting(c, CLAUDE_FOLDERS_SETTING))
        .ok()
        .flatten()
        .and_then(|json| serde_json::from_str::<Vec<PathBuf>>(&json).ok())
        .unwrap_or_default()
}

fn init_logging(app: &App) -> anyhow::Result<WorkerGuard> {
    use tracing_subscriber::{fmt, prelude::*, EnvFilter};

    let log_dir = app.path().app_log_dir()?;
    std::fs::create_dir_all(&log_dir)?;
    let file = tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix("agentpulse")
        .filename_suffix("log")
        .max_log_files(7)
        .build(&log_dir)?;
    let (writer, guard) = tracing_appender::non_blocking(file);
    let filter = EnvFilter::try_from_env("AGENTPULSE_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
    let registry = tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().with_writer(writer).with_ansi(false));
    #[cfg(debug_assertions)]
    let registry = registry.with(fmt::layer().with_writer(std::io::stderr));
    registry.try_init()?;

    // Route panics through tracing (message + location only; payloads from our code carry no content).
    std::panic::set_hook(Box::new(|info| {
        let location = info.location().map(|l| format!("{}:{}", l.file(), l.line())).unwrap_or_default();
        let message = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| (*s).to_owned())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_default();
        tracing::error!(%location, %message, "panic");
    }));
    Ok(guard)
}
