//! `dlog commit` — run `git commit`, then seal staged decisions to the new
//! commit (design §8.3 code path; v0.2).
//!
//! This removes the "agent forgot to bind" gap: sealing happens automatically as
//! part of committing. git commit runs first; only on success do we read the new
//! HEAD and seal staging to it. git's own output is captured (not echoed) so the
//! command keeps the one-JSON-document contract; on failure its stderr is
//! surfaced in the error.

use serde::Serialize;

use crate::cli::CommitArgs;
use crate::commands::{AppError, current_git_sha, open_store};
use crate::model::Binding;
use crate::output::emit;
use crate::store::Store;

/// Success document for `dlog commit`.
#[derive(Debug, Serialize)]
struct CommitResult {
    sha: String,
    count: usize,
    sealed: Vec<String>,
}

pub fn run(args: CommitArgs) -> Result<(), AppError> {
    // Run git commit first; only seal if it succeeds, so staging is never
    // sealed to a commit that didn't happen.
    let output = std::process::Command::new("git")
        .arg("commit")
        .args(&args.git_args)
        .output()
        .map_err(|e| AppError::new("git_unavailable", e.to_string()))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let message = if stderr.is_empty() {
            "git commit failed".to_string()
        } else {
            stderr
        };
        return Err(AppError::new("git_commit_failed", message));
    }

    let sha = current_git_sha()
        .ok_or_else(|| AppError::new("git_no_head", "could not read HEAD after commit"))?;

    let store = open_store(args.db)?;
    let only = if args.decisions.is_empty() {
        None
    } else {
        Some(args.decisions.as_slice())
    };

    emit(&seal_to_commit(&store, &sha, only)?);
    Ok(())
}

/// Seal staged decisions to a commit sha. Separated from the git invocation so
/// the seal behaviour is testable without running `git commit`.
fn seal_to_commit(
    store: &Store,
    sha: &str,
    only: Option<&[String]>,
) -> rusqlite::Result<CommitResult> {
    let sealed = store.seal_staged(
        &Binding::Commit {
            sha: sha.to_string(),
        },
        only,
    )?;
    Ok(CommitResult {
        sha: sha.to_string(),
        count: sealed.len(),
        sealed,
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::model::{Agent, Anchor, NewDecision};

    fn temp_db() -> PathBuf {
        std::env::temp_dir().join(format!("dlog-commit-{}.db", ulid::Ulid::new()))
    }

    fn stage(store: &Store, rationale: &str) -> String {
        store
            .stage_decision(&NewDecision {
                task_id: None,
                agent: Agent {
                    role: "implementer".into(),
                    model: "claude-test".into(),
                    session_id: None,
                },
                conversation_id: None,
                rationale: rationale.into(),
                rejected: vec![],
                caused_by: vec![],
                supersedes: None,
                anchors: vec![Anchor {
                    file: "src/lib.rs".into(),
                    symbol_path: None,
                    node_kind: None,
                    structural_hash: None,
                    line_span: None,
                    recorded_at_sha: None,
                }],
            })
            .unwrap()
    }

    // The git-invocation glue isn't unit-tested (running `git commit` has side
    // effects on the repo); the dlog-specific seal-to-commit step is.
    #[test]
    fn seal_to_commit_binds_all_staged_to_the_sha() {
        let db = temp_db();
        let store = Store::open(&db).unwrap();
        let a = stage(&store, "one");
        let b = stage(&store, "two");

        let result = seal_to_commit(&store, "deadbeef", None).unwrap();
        assert_eq!(result.sha, "deadbeef");
        assert_eq!(result.count, 2);

        for id in [&a, &b] {
            let d = store.get_decision(id).unwrap().unwrap();
            assert!(!d.staged);
            assert_eq!(
                d.binding,
                Some(Binding::Commit {
                    sha: "deadbeef".into()
                })
            );
        }
        assert_eq!(store.status().unwrap().staging_count, 0);
        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn seal_to_commit_empty_staging_is_a_no_op() {
        let db = temp_db();
        let store = Store::open(&db).unwrap();
        let result = seal_to_commit(&store, "abc123", None).unwrap();
        assert_eq!(result.count, 0);
        assert!(result.sealed.is_empty());
        let _ = std::fs::remove_file(&db);
    }
}
