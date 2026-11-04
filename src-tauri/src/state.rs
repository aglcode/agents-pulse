//! App state: logging, database, engine, event bridge.

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
#[cfg(debug_assertions)]
use std::time::Duration;

use agentpulse_core::collector::Collector;
use agentpulse_engine::{Engine, EngineConfig};
use tauri::{App, Manager};
use tracing_appender::non_blocking::WorkerGuard;

pub struct AppState {
    pub engine: Arc<Engine>,
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

    let config = EngineConfig { home_dir: app.path().home_dir().ok(), ..EngineConfig::default() };
    // Reuse Tauri's Tokio runtime; the engine never creates its own.
    let rt = tauri::async_runtime::handle().inner().clone();
    // Opt-in: only collectors the user enabled earlier start now.
    let enabled = Engine::enabled_from_settings(&reader);
    tracing::info!(enabled = enabled.len(), "collectors enabled from settings");
    let engine = Arc::new(Engine::start(&rt, collectors(), &enabled, writer, reader, config));

    crate::events::spawn_bridge(app.handle().clone(), engine.clone());

    app.manage(AppState {
        engine,
        log_guard: Mutex::new(Some(log_guard)),
        shutting_down: AtomicBool::new(false),
        shutdown_done: AtomicBool::new(false),
    });
    Ok(())
}

/// Real provider collectors are added from M3. Debug builds run a synthetic
/// collector so the pipeline can be exercised end to end; release builds never do.
fn collectors() -> Vec<Arc<dyn Collector>> {
    #[cfg(debug_assertions)]
    {
        use agentpulse_collectors::mock::{MockBehavior, MockCollector};
        vec![Arc::new(MockCollector::new("mock-healthy", MockBehavior::Healthy, Duration::from_secs(15)))]
    }
    #[cfg(not(debug_assertions))]
    {
        Vec::new()
    }
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
