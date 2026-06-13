//! JSON output contract shared by every `dlog` command.
//!
//! dlog is consumed by agents, not humans (design §6.1): every invocation emits
//! exactly one JSON document on stdout. Success and failure share that channel;
//! a caller distinguishes them by the process exit code and by whether the
//! document carries a top-level `error`.

use serde::Serialize;

/// Command succeeded.
pub const EXIT_OK: i32 = 0;
/// Command ran but failed; the emitted document carries an `error`. Usage errors
/// are reported separately by clap with exit code 2.
pub const EXIT_ERROR: i32 = 1;

/// Body of a failure response: a stable machine `code` agents branch on, plus a
/// human-readable `message` for the transcript.
#[derive(Debug, Serialize)]
pub struct ErrorBody {
    pub code: String,
    pub message: String,
}

/// Top-level failure document: `{ "error": { "code", "message" } }`.
#[derive(Debug, Serialize)]
pub struct ErrorEnvelope {
    pub error: ErrorBody,
}

impl ErrorEnvelope {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            error: ErrorBody {
                code: code.into(),
                message: message.into(),
            },
        }
    }
}

/// Query response envelope (design §9.3). `Q` is the interpreted query and `R` a
/// single compact result row; full records are fetched separately via
/// `dlog show` (two-stage retrieval, §9.1). `resolved` carries anchor-resolution
/// metadata and is omitted for queries that don't resolve an anchor.
///
/// This type only fixes the wire shape; the matching that fills `resolution`
/// lands with the anchor resolver (#8).
#[derive(Debug, Serialize)]
pub struct QueryEnvelope<Q, R> {
    pub query: Q,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved: Option<Resolved>,
    pub results: Vec<R>,
    pub truncated: bool,
}

/// Anchor-resolution metadata surfaced alongside query results (§9.3).
#[derive(Debug, Serialize)]
pub struct Resolved {
    pub node: String,
    pub resolution: Resolution,
}

/// Confidence that a query's anchor still points at the recorded node (§10.3).
/// The judgement that selects a variant is the anchor resolver's job (#8); the
/// enum lives here because it is part of the query wire format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Resolution {
    Exact,
    Drifted,
    Relocated,
    FileFallback,
}

/// Serialize `value` as a single compact JSON line on stdout.
pub fn emit<T: Serialize>(value: &T) {
    // Serializing our own owned types should never fail; if it somehow does,
    // fall back to a hand-built error document so the JSON contract still holds.
    let json = serde_json::to_string(value).unwrap_or_else(|e| {
        serde_json::to_string(&ErrorEnvelope::new("serialize", e.to_string()))
            .unwrap_or_else(|_| String::from(r#"{"error":{"code":"serialize","message":""}}"#))
    });
    println!("{json}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_envelope_shape() {
        let env = ErrorEnvelope::new("not_implemented", "nope");
        let v = serde_json::to_value(&env).unwrap();
        assert_eq!(v["error"]["code"], "not_implemented");
        assert_eq!(v["error"]["message"], "nope");
    }

    #[test]
    fn resolution_serializes_snake_case() {
        let v = serde_json::to_value(Resolution::FileFallback).unwrap();
        assert_eq!(v, serde_json::json!("file_fallback"));
    }

    #[test]
    fn query_envelope_omits_resolved_when_none() {
        let env: QueryEnvelope<&str, &str> = QueryEnvelope {
            query: "why",
            resolved: None,
            results: vec![],
            truncated: false,
        };
        let v = serde_json::to_value(&env).unwrap();
        assert!(v.get("resolved").is_none());
        assert_eq!(v["truncated"], false);
    }
}
