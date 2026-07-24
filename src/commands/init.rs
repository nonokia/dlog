//! `dlog init` — declare the current directory as a workspace root.
//!
//! Root discovery falls back to `.git` and then to the current directory, so
//! `init` is never required — but it is what makes the root a dlog concept
//! rather than a git one, which is the whole point outside a repository.
//!
//! Idempotent: the schema migration is idempotent DDL replay, so re-running only
//! flips `created` to false.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::cli::InitArgs;
use crate::commands::{AppError, DLOG_DIR, RootSource, Workspace};
use crate::output::emit;

/// Success document for `dlog init`.
#[derive(Debug, Serialize)]
struct InitResult {
    root: String,
    db: String,
    /// False when the store already existed (re-running `init` is harmless).
    created: bool,
    /// Set when an ancestor already holds a `.dlog/`, so this new root will
    /// shadow it for anything run below here. Reported as state, not as advice
    /// (§9.1 principle 2) — nesting workspaces is legal, just easy to do by
    /// accident.
    #[serde(skip_serializing_if = "Option::is_none")]
    shadows: Option<String>,
}

pub fn run(args: InitArgs) -> Result<(), AppError> {
    let cwd = std::env::current_dir()?;
    emit(&init_at(&cwd, args.db)?);
    Ok(())
}

fn init_at(cwd: &Path, db: Option<String>) -> Result<InitResult, AppError> {
    // `init` creates a root rather than finding one, so it deliberately skips
    // discovery: running it inside an existing workspace nests a new one.
    let workspace = Workspace::rooted(cwd.to_path_buf(), RootSource::Dlog, db);
    let created = !workspace.db.exists();
    workspace.open()?;

    Ok(InitResult {
        root: workspace.root.display().to_string(),
        db: workspace.db.display().to_string(),
        created,
        shadows: outer_workspace(cwd).map(|p| p.display().to_string()),
    })
}

/// The nearest ancestor *above* `dir` that already holds a `.dlog/`, if any.
fn outer_workspace(dir: &Path) -> Option<PathBuf> {
    dir.ancestors()
        .skip(1)
        .find(|d| d.join(DLOG_DIR).is_dir())
        .map(Path::to_path_buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("dlog-init-{tag}-{}", ulid::Ulid::new()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::canonicalize(&dir).unwrap()
    }

    #[test]
    fn init_creates_the_store_and_is_idempotent() {
        let dir = temp_dir("fresh");

        let first = init_at(&dir, None).unwrap();
        assert!(first.created);
        assert_eq!(first.root, dir.display().to_string());
        assert!(dir.join(DLOG_DIR).join("dlog.db").exists());
        assert!(first.shadows.is_none());

        let second = init_at(&dir, None).unwrap();
        assert!(!second.created, "re-running init must not claim creation");
        assert_eq!(second.db, first.db);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn init_reports_an_outer_workspace_it_shadows() {
        let outer = temp_dir("outer");
        std::fs::create_dir_all(outer.join(DLOG_DIR)).unwrap();
        let inner = outer.join("sub/project");
        std::fs::create_dir_all(&inner).unwrap();

        let result = init_at(&inner, None).unwrap();
        assert_eq!(
            result.shadows.as_deref(),
            Some(outer.display().to_string()).as_deref()
        );

        let _ = std::fs::remove_dir_all(&outer);
    }

    #[test]
    fn init_honours_an_explicit_db_path() {
        let dir = temp_dir("explicit");
        let db = dir.join("custom.db");

        let result = init_at(&dir, Some(db.display().to_string())).unwrap();
        assert!(result.created);
        assert_eq!(result.db, db.display().to_string());
        assert!(db.exists());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
