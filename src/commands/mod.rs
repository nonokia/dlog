//! Command handlers. Each subcommand's behaviour lives in its own module; the
//! dispatch table in `lib.rs` routes parsed args here.

pub mod record;

use crate::output::ErrorEnvelope;

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
