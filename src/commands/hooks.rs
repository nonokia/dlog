//! `dlog hooks` — install/uninstall a git `post-commit` hook that auto-seals
//! staging to the new commit (design §8.3; v0.2, #27).
//!
//! Unlike `dlog commit` (which the agent must invoke), this is a repository-side
//! mechanism: a plain `git commit` — from any tool — fires the hook, which runs
//! `dlog bind <new HEAD>`. That removes the reliance on the agent remembering to
//! seal. Optional, idempotent, and reversible; an existing hook is preserved (we
//! append a marked block) and the seal is best-effort (`|| true`) so it can never
//! block a commit.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Serialize;

use crate::cli::{HookAction, HooksArgs};
use crate::commands::AppError;
use crate::output::emit;

const BEGIN: &str = "# >>> dlog managed block >>>";
const END: &str = "# <<< dlog managed block <<<";

#[derive(Debug, Serialize)]
struct HooksResult {
    action: &'static str,
    hook: String,
    status: &'static str,
}

pub fn run(args: HooksArgs) -> Result<(), AppError> {
    let hooks_dir = git_hooks_dir()?;
    std::fs::create_dir_all(&hooks_dir)?;
    let hook_path = hooks_dir.join("post-commit");

    let status = match args.action {
        HookAction::Install => {
            let bin =
                std::env::current_exe().map_err(|e| AppError::new("no_exe_path", e.to_string()))?;
            // Bake an absolute store path only when one is configured, so the
            // hook targets the right store even if a commit happens from an
            // environment without $DLOG_DB. With no explicit store the hook
            // omits --db and uses the default `.dlog/dlog.db` relative to the
            // repo root, which survives the repo being moved.
            let db = args.db.as_deref().map(absolute_db);
            install(&hook_path, &bin.to_string_lossy(), db.as_deref())?
        }
        HookAction::Uninstall => uninstall(&hook_path)?,
    };

    emit(&HooksResult {
        action: args.action.as_str(),
        hook: hook_path.display().to_string(),
        status,
    });
    Ok(())
}

/// The repository's hooks directory, honouring worktrees / `core.hooksPath`.
fn git_hooks_dir() -> Result<PathBuf, AppError> {
    let out = Command::new("git")
        .args(["rev-parse", "--git-path", "hooks"])
        .output()
        .map_err(|e| AppError::new("git_unavailable", e.to_string()))?;
    if !out.status.success() {
        return Err(AppError::new(
            "not_a_git_repo",
            "not inside a git repository",
        ));
    }
    let dir = String::from_utf8_lossy(&out.stdout).trim().to_string();
    Ok(PathBuf::from(dir))
}

/// Absolutize a store path (lexically, without touching the filesystem) so the
/// hook isn't sensitive to the cwd at commit time.
fn absolute_db(path: &str) -> String {
    let p = PathBuf::from(path);
    std::path::absolute(&p)
        .unwrap_or(p)
        .to_string_lossy()
        .into_owned()
}

/// The managed hook block, invoking this dlog binary by absolute path so the
/// hook works regardless of `PATH`. When `db` is set, it is baked in so the hook
/// always targets that store.
fn managed_block(bin: &str, db: Option<&str>) -> String {
    let db_arg = match db {
        Some(path) => format!(" --db \"{path}\""),
        None => String::new(),
    };
    format!(
        "{BEGIN}\n\"{bin}\" bind{db_arg} \"$(git rev-parse HEAD)\" >/dev/null 2>&1 || true\n{END}\n"
    )
}

fn install(hook_path: &Path, bin: &str, db: Option<&str>) -> std::io::Result<&'static str> {
    let block = managed_block(bin, db);
    if hook_path.exists() {
        let existing = std::fs::read_to_string(hook_path)?;
        if existing.contains(BEGIN) {
            return Ok("already_installed");
        }
        // Preserve the user's existing hook; append our marked block.
        let mut content = existing;
        if !content.ends_with('\n') {
            content.push('\n');
        }
        content.push('\n');
        content.push_str(&block);
        std::fs::write(hook_path, content)?;
        make_executable(hook_path)?;
        Ok("appended")
    } else {
        std::fs::write(hook_path, format!("#!/bin/sh\n{block}"))?;
        make_executable(hook_path)?;
        Ok("installed")
    }
}

fn uninstall(hook_path: &Path) -> std::io::Result<&'static str> {
    if !hook_path.exists() {
        return Ok("not_installed");
    }
    let existing = std::fs::read_to_string(hook_path)?;
    let Some(stripped) = strip_block(&existing) else {
        return Ok("not_installed");
    };
    // If only the shebang/whitespace remains, remove the file; else write back
    // the user's surviving hook.
    let has_other_content = stripped
        .lines()
        .any(|l| !l.trim().is_empty() && !l.trim_start().starts_with("#!"));
    if has_other_content {
        std::fs::write(hook_path, stripped)?;
    } else {
        std::fs::remove_file(hook_path)?;
    }
    Ok("removed")
}

/// Remove the managed block (markers inclusive) from `content`, returning the
/// remainder, or `None` if no block is present.
fn strip_block(content: &str) -> Option<String> {
    let begin = content.find(BEGIN)?;
    let end = content.find(END)? + END.len();
    let mut tail = &content[end..];
    tail = tail.strip_prefix('\n').unwrap_or(tail);
    // Drop a blank separator line we may have inserted before the block.
    let mut head = content[..begin].to_string();
    while head.ends_with("\n\n") {
        head.pop();
    }
    Some(format!("{head}{tail}"))
}

#[cfg(unix)]
fn make_executable(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path)?.permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms)
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_hook() -> PathBuf {
        std::env::temp_dir().join(format!("dlog-hook-{}", ulid::Ulid::new()))
    }

    #[test]
    fn install_creates_then_is_idempotent() {
        let path = temp_hook();
        assert_eq!(install(&path, "/usr/bin/dlog", None).unwrap(), "installed");
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.starts_with("#!/bin/sh"));
        assert!(content.contains(BEGIN) && content.contains(END));
        assert!(content.contains("/usr/bin/dlog"));
        assert!(!content.contains("--db"), "no --db when none configured");

        // Second install is a no-op.
        assert_eq!(
            install(&path, "/usr/bin/dlog", None).unwrap(),
            "already_installed"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn install_bakes_db_path_when_configured() {
        let path = temp_hook();
        install(&path, "/usr/bin/dlog", Some("/abs/store.db")).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(
            content.contains("--db \"/abs/store.db\""),
            "configured store baked into the hook: {content}"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn install_appends_to_existing_hook_and_uninstall_preserves_it() {
        let path = temp_hook();
        std::fs::write(&path, "#!/bin/sh\necho existing\n").unwrap();

        assert_eq!(install(&path, "/usr/bin/dlog", None).unwrap(), "appended");
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("echo existing"), "user hook preserved");
        assert!(content.contains(BEGIN));

        // Uninstall removes only our block, keeping the user's hook + file.
        assert_eq!(uninstall(&path).unwrap(), "removed");
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("echo existing"));
        assert!(!content.contains(BEGIN));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn uninstall_removes_file_when_only_ours() {
        let path = temp_hook();
        install(&path, "/usr/bin/dlog", None).unwrap();
        assert_eq!(uninstall(&path).unwrap(), "removed");
        assert!(!path.exists(), "file removed when nothing else remained");
    }

    #[test]
    fn uninstall_when_absent_or_unmanaged() {
        let path = temp_hook();
        assert_eq!(uninstall(&path).unwrap(), "not_installed");
        std::fs::write(&path, "#!/bin/sh\necho hi\n").unwrap();
        assert_eq!(uninstall(&path).unwrap(), "not_installed");
        let _ = std::fs::remove_file(&path);
    }
}
