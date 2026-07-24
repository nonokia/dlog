//! Command handlers. Each subcommand's behaviour lives in its own module; the
//! dispatch table in `lib.rs` routes parsed args here.

pub mod bind;
pub mod commit;
pub mod compact;
pub mod context;
pub mod hooks;
pub mod init;
pub mod invariants;
pub mod record;
pub mod search;
pub mod show;
pub mod status;
pub mod trace;
pub mod why;

use std::path::{Component, Path, PathBuf};

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

/// The directory name that marks — and holds — a dlog workspace.
pub(crate) const DLOG_DIR: &str = ".dlog";

/// How the workspace root was determined. Reported by `status` / `init` so the
/// agent can see *why* a given directory is the root (§9.1 principle 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RootSource {
    /// An ancestor declared itself with a `.dlog/` directory.
    Dlog,
    /// No `.dlog/`, but an ancestor is a git repository.
    Git,
    /// Neither marker exists; the current directory is the root.
    Cwd,
}

/// Find the workspace root above `from`: the nearest ancestor with a `.dlog/`
/// directory, else the nearest ancestor that is a git repository, else `from`.
///
/// `.dlog/` is searched across the *whole* ancestor chain before `.git` is
/// considered, so a project that declared its own root wins over a nested
/// repository — that is what keeps the root a dlog concept rather than a git one.
fn discover_root(from: &Path) -> (PathBuf, RootSource) {
    for dir in from.ancestors() {
        if dir.join(DLOG_DIR).is_dir() {
            return (dir.to_path_buf(), RootSource::Dlog);
        }
    }
    // `.git` is a directory in a normal checkout and a file in a worktree or
    // submodule, so test for existence rather than for a directory.
    for dir in from.ancestors() {
        if dir.join(".git").exists() {
            return (dir.to_path_buf(), RootSource::Git);
        }
    }
    (from.to_path_buf(), RootSource::Cwd)
}

/// The workspace a command operates in: where the project root is, which store
/// answers, and how the root was found.
///
/// Anchors are stored root-relative, so every command has to agree on the root
/// no matter which directory the agent invoked dlog from — without that, running
/// from a subdirectory silently forks the log into a second store.
#[derive(Debug, Clone)]
pub(crate) struct Workspace {
    pub root: PathBuf,
    pub db: PathBuf,
    pub root_source: RootSource,
}

impl Workspace {
    /// Discover the workspace from the process's current directory.
    pub fn discover(db: Option<String>) -> Result<Self, AppError> {
        let cwd = std::env::current_dir()?;
        let (root, root_source) = discover_root(&cwd);
        Ok(Self::rooted(root, root_source, db))
    }

    /// A workspace at an explicit root, skipping discovery. Used by `init` (which
    /// *creates* a root rather than finding one) and by tests, which must not
    /// depend on the process-global current directory.
    pub fn rooted(root: PathBuf, root_source: RootSource, db: Option<String>) -> Self {
        let db = db
            .map(PathBuf::from)
            .unwrap_or_else(|| root.join(DLOG_DIR).join("dlog.db"));
        Self {
            root,
            db,
            root_source,
        }
    }

    /// Open (creating if needed) this workspace's store, ensuring the parent
    /// directory exists.
    pub fn open(&self) -> Result<Store, AppError> {
        if let Some(parent) = self.db.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }
        Ok(Store::open(&self.db)?)
    }

    /// The stored spelling of a caller-supplied path: root-relative and
    /// `/`-separated, so the same file gets the same anchor from any directory.
    pub fn relativize(&self, cwd: &Path, path: &str) -> String {
        relativize(&self.root, cwd, path)
    }

    /// Turn a stored (root-relative) path back into one that can be read.
    pub fn resolve_path(&self, stored: &str) -> PathBuf {
        if stored == "." {
            return self.root.clone();
        }
        let path = Path::new(stored);
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.root.join(path)
        }
    }
}

/// Normalize `path` (interpreted relative to `cwd`) into its root-relative,
/// `/`-separated stored form. The workspace root itself becomes `"."`.
///
/// Purely lexical — it never touches the filesystem, because an anchor may name
/// a file that does not exist yet, and `canonicalize` would also resolve
/// symlinks and so make the stored spelling machine-dependent. A path that lands
/// outside the workspace stays absolute rather than being rewritten into
/// something misleading.
fn relativize(root: &Path, cwd: &Path, path: &str) -> String {
    let raw = Path::new(path);
    let absolute = if raw.is_absolute() {
        raw.to_path_buf()
    } else {
        cwd.join(raw)
    };
    let folded = fold(&absolute);
    let relative = folded.strip_prefix(root).unwrap_or(&folded);
    let out = to_slash(relative);
    if out.is_empty() { ".".to_string() } else { out }
}

/// Fold `.` and `..` components lexically. A `..` with nothing left to pop is
/// kept, which can only happen for paths reaching above the filesystem root.
fn fold(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Render a path with `/` separators so stored anchors are platform-independent.
fn to_slash(path: &Path) -> String {
    let mut out = String::new();
    for component in path.components() {
        match component {
            Component::RootDir => out.push('/'),
            other => {
                if !out.is_empty() && !out.ends_with('/') {
                    out.push('/');
                }
                out.push_str(&other.as_os_str().to_string_lossy());
            }
        }
    }
    out
}

/// The current git commit (`git rev-parse HEAD`) of the working directory, or
/// `None` outside a git repo / when git is unavailable. Best-effort: commands
/// that record the base commit must not depend on git. Shared by `record`
/// (recorded_at_sha) and `commit` (sha to seal against).
pub(crate) fn current_git_sha() -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let sha = String::from_utf8(output.stdout).ok()?.trim().to_string();
    (!sha.is_empty()).then_some(sha)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A throwaway directory tree. Root discovery is filesystem-dependent, so
    /// these tests need real directories — but never the process cwd.
    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("dlog-{tag}-{}", ulid::Ulid::new()));
        std::fs::create_dir_all(&dir).unwrap();
        // Resolve symlinked temp dirs (e.g. /tmp -> /private/tmp on macOS) so
        // `strip_prefix` comparisons line up with the paths we build from it.
        std::fs::canonicalize(&dir).unwrap()
    }

    #[test]
    fn discover_root_prefers_dlog_over_a_nearer_git() {
        let root = temp_dir("root");
        std::fs::create_dir_all(root.join(DLOG_DIR)).unwrap();
        let nested = root.join("vendor/lib");
        std::fs::create_dir_all(nested.join(".git")).unwrap();

        let (found, source) = discover_root(&nested);
        assert_eq!(found, root);
        assert_eq!(source, RootSource::Dlog);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn discover_root_falls_back_to_git_then_cwd() {
        let root = temp_dir("git-root");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        let nested = root.join("src/auth");
        std::fs::create_dir_all(&nested).unwrap();

        let (found, source) = discover_root(&nested);
        assert_eq!(found, root);
        assert_eq!(source, RootSource::Git);

        // A bare directory with no marker anywhere above it is its own root.
        // (`/tmp` itself carries no `.dlog`/`.git`, so the walk reaches the top.)
        let bare = temp_dir("bare");
        let (found, source) = discover_root(&bare);
        assert_eq!(found, bare);
        assert_eq!(source, RootSource::Cwd);

        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&bare);
    }

    #[test]
    fn relativize_normalizes_against_the_root() {
        let root = Path::new("/proj");
        let sub = Path::new("/proj/src");

        // The whole point: the same file from a subdirectory and from the root.
        assert_eq!(relativize(root, sub, "auth.rs"), "src/auth.rs");
        assert_eq!(relativize(root, root, "src/auth.rs"), "src/auth.rs");
        // Absolute inside the root, and `.`/`..` folded lexically.
        assert_eq!(relativize(root, sub, "/proj/src/auth.rs"), "src/auth.rs");
        assert_eq!(relativize(root, sub, "./auth.rs"), "src/auth.rs");
        assert_eq!(relativize(root, sub, "../README.md"), "README.md");
        assert_eq!(relativize(root, sub, "../src/../src/a.rs"), "src/a.rs");
        // The root itself is spelled `.` (so `dlog context .` means everything).
        assert_eq!(relativize(root, sub, ".."), ".");
        assert_eq!(relativize(root, root, "."), ".");
        // Outside the workspace: kept absolute rather than rewritten.
        assert_eq!(relativize(root, sub, "/etc/hosts"), "/etc/hosts");
        assert_eq!(
            relativize(root, sub, "../../elsewhere/x.rs"),
            "/elsewhere/x.rs"
        );
    }

    #[test]
    fn relativize_does_not_require_the_path_to_exist() {
        // Anchors routinely name files the agent is about to create.
        let root = Path::new("/proj");
        assert_eq!(
            relativize(root, root, "src/not/created/yet.rs"),
            "src/not/created/yet.rs"
        );
    }

    #[test]
    fn resolve_path_round_trips_a_stored_anchor() {
        let ws = Workspace::rooted(PathBuf::from("/proj"), RootSource::Cwd, None);
        assert_eq!(
            ws.resolve_path("src/auth.rs"),
            PathBuf::from("/proj/src/auth.rs")
        );
        assert_eq!(ws.resolve_path("."), PathBuf::from("/proj"));
        assert_eq!(ws.resolve_path("/etc/hosts"), PathBuf::from("/etc/hosts"));
    }

    #[test]
    fn workspace_db_defaults_under_the_root_and_honours_an_override() {
        let ws = Workspace::rooted(PathBuf::from("/proj"), RootSource::Git, None);
        assert_eq!(ws.db, PathBuf::from("/proj/.dlog/dlog.db"));

        let ws = Workspace::rooted(
            PathBuf::from("/proj"),
            RootSource::Git,
            Some("/elsewhere/other.db".into()),
        );
        assert_eq!(ws.db, PathBuf::from("/elsewhere/other.db"));
        assert_eq!(
            ws.root,
            PathBuf::from("/proj"),
            "--db does not move the root"
        );
    }

    #[test]
    fn current_git_sha_is_hex_when_present() {
        // In a git checkout this is Some(hex); outside one it's None. Either is
        // acceptable — we only assert the shape when present.
        if let Some(sha) = current_git_sha() {
            assert!(!sha.is_empty());
            assert!(
                sha.chars().all(|c| c.is_ascii_hexdigit()),
                "sha should be hex: {sha}"
            );
        }
    }
}
