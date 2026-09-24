//! The daemon's own diagnostic log (not the audit log).

use std::path::Path;

use anyhow::Result;

/// Log to stderr, or append to `file`. `CWW_LOG` sets the filter (default
/// `info`).
pub fn init(file: Option<&Path>) -> Result<()> {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_env("CWW_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false);
    match file {
        None => builder.init(),
        Some(path) => {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            // Keep one previous log; the daemon logs little.
            if std::fs::metadata(path).is_ok_and(|m| m.len() > 5 * 1024 * 1024) {
                let _ = std::fs::rename(path, path.with_extension("log.1"));
            }
            let file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)?;
            builder
                .with_ansi(false)
                .with_writer(std::sync::Mutex::new(file))
                .init();
        }
    }
    Ok(())
}
