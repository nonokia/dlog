//! Command handlers. Each subcommand's behaviour lives in its own module; the
//! dispatch table in `lib.rs` routes parsed args here.

pub mod bind;
pub mod compact;
pub mod invariants;
pub mod record;
pub mod search;
pub mod show;
pub mod status;
pub mod why;

use std::path::{Path, PathBuf};

use crate::output::ErrorEnvelope;
use crate::store::Store;

/// Parse a line spec — `N` or `START-END` — into a 1-based inclusive span.
/// Shared by anchor parsing (`record`) and query parsing (`why`).
pub(crate) fn parse_line_spec(s: &str) -> Option<(u32, u32)> {
    match s.split_once('-') {
        Some((a, b)) => Some((a.trim().parse().ok()?, b.trim().parse().ok()?)),
        None => {
            let n = s.trim().parse().ok()?;
            Some((n, n))
        }
    }
}

/// A command failure, carrying a stable machine `code` and a human message.
/// Converted to an [`ErrorEnvelope`] for emission (design §9.3).
#[derive(Debug)]
pub struct AppError {
    pub code: String,
    pub message: String,
}

impl AppError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }

    /// Uniform stub error for commands not yet implemented.
    pub fn not_implemented(command: &str, issue: u32) -> Self {
        Self::new(
            "not_implemented",
            format!("`dlog {command}` is not implemented yet (tracked in #{issue})"),
        )
    }

    pub fn into_envelope(self) -> ErrorEnvelope {
        ErrorEnvelope::new(self.code, self.message)
    }
}

impl From<rusqlite::Error> for AppError {
    fn from(e: rusqlite::Error) -> Self {
        AppError::new("store_error", e.to_string())
    }
}

impl From<std::io::Error> for AppError {
    fn from(e: std::io::Error) -> Self {
        AppError::new("io_error", e.to_string())
    }
}

/// Resolve the store path: explicit `--db`/`$DLOG_DB`, else `.dlog/dlog.db`.
fn resolve_db(arg: Option<String>) -> PathBuf {
    arg.map(PathBuf::from)
        .unwrap_or_else(|| Path::new(".dlog").join("dlog.db"))
}

/// Open (creating if needed) the store at the resolved path, ensuring the parent
/// directory exists. Shared by commands that touch the store.
pub(crate) fn open_store(db: Option<String>) -> Result<Store, AppError> {
    let path = resolve_db(db);
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    Ok(Store::open(&path)?)
}
