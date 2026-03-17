use eyre::{Context, Result};
use std::fs;
use std::path::PathBuf;
use tracing_subscriber::EnvFilter;

/// Resolve log level with precedence:
/// CLI --log-level > OBSIDIAN_CORTEX_LOG env var > config log-level > info
pub fn resolve_log_level(cli_level: Option<&str>, config_level: &str) -> String {
    if let Some(level) = cli_level {
        return level.to_string();
    }
    if let Ok(env_level) = std::env::var("OBSIDIAN_CORTEX_LOG") {
        return env_level;
    }
    config_level.to_string()
}

/// Setup tracing subscriber with file + stderr output.
pub fn setup_tracing(level: &str) -> Result<()> {
    let log_dir = log_dir();
    fs::create_dir_all(&log_dir).context("failed to create log directory")?;

    let file_appender = tracing_appender::rolling::daily(&log_dir, "obsidian-cortex.log");
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

    // Leak the guard so it lives for the program's lifetime
    std::mem::forget(guard);

    let filter = EnvFilter::try_new(level).unwrap_or_else(|_| EnvFilter::new("info"));

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(move || -> Box<dyn std::io::Write> { Box::new(non_blocking.clone()) })
        .with_target(true)
        .with_thread_ids(false)
        .with_ansi(false)
        .init();

    Ok(())
}

/// Return the XDG-compliant log directory path.
pub fn log_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("obsidian-cortex")
        .join("logs")
}
