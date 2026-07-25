//! `dlog record` — write a decision into staging (design §7, §8.2).
//!
//! Decisions are born before a commit, so recording goes straight to the
//! mutable staging area; binding happens later at seal time (#6). This handler
//! maps CLI args to a [`NewDecision`], opens the store, and stages it.

use serde::Serialize;

use crate::cli::RecordArgs;
use crate::commands::{AppError, DLOG_DIR, Workspace, current_git_sha, parse_line_spec};
use crate::model::{Agent, Anchor, NewDecision, Rejected};
use crate::output::emit;

/// Success document for `dlog record`.
#[derive(Debug, Serialize)]
struct RecordResult {
    id: String,
    /// Always true: a freshly recorded decision starts in staging (§8.2).
    staged: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    declared_invariants: Vec<String>,
}

pub fn run(args: RecordArgs) -> Result<(), AppError> {
    // Rationale: "-" reads from stdin so agents can pipe long prose unquoted.
    let rationale = resolve_rationale(&args.rationale)?;

    // Anchors are stored root-relative so the same file resolves identically
    // whichever directory the agent recorded from.
    let workspace = Workspace::discover(args.db)?;
    let cwd = std::env::current_dir()?;

    let mut anchors: Vec<Anchor> = args
        .files
        .iter()
        .map(|s| parse_anchor(s, &workspace, &cwd))
        .collect();
    // --changed: infer file-level anchors from the working tree, so a decision
    // about the current changes needn't list each file (lower friction, §7.3).
    // Union with explicit --file, de-duplicated by path.
    if args.changed {
        let have: std::collections::HashSet<String> =
            anchors.iter().map(|a| a.file.clone()).collect();
        for file in git_changed_files(&workspace, &cwd) {
            if !have.contains(&file) {
                anchors.push(file_anchor(file));
            }
        }
    }
    // An anchor is part of the minimal required surface (§7.3).
    if anchors.is_empty() {
        return Err(AppError::new(
            "missing_anchor",
            "record requires an anchor: pass --file or --changed (design §7.3)",
        ));
    }

    // The base commit the agent is looking at while recording (§10.2). Distinct
    // from the binding stamped at seal, which may be a later commit. Best-effort.
    let recorded_at_sha = current_git_sha();
    // Capture the AST-node observation now, while we're looking at the code
    // (§10.2). Best-effort: anything that doesn't resolve stays a file-level
    // anchor (§10.5), recording never fails because of it.
    for anchor in &mut anchors {
        enrich_anchor(anchor, &workspace);
        if anchor.recorded_at_sha.is_none() {
            anchor.recorded_at_sha = recorded_at_sha.clone();
        }
    }
    let rejected: Vec<Rejected> = args.rejected.iter().map(|s| parse_rejected(s)).collect();

    let decision = NewDecision {
        task_id: args.task_id.clone(),
        agent: Agent {
            role: args.agent_role,
            model: args.agent_model,
            session_id: args.agent_session,
        },
        conversation_id: args.conversation_id,
        rationale,
        rejected,
        caused_by: args.caused_by,
        supersedes: args.supersedes,
        anchors,
    };

    let store = workspace.open()?;

    // A decision may reference a task; make sure the row exists so the FK holds.
    if let Some(task_id) = decision.task_id.as_deref() {
        store.ensure_task(task_id, args.instruction.as_deref())?;
    }

    let id = store.stage_decision(&decision)?;

    let declared_invariants = args
        .declares_invariant
        .iter()
        .map(|statement| store.insert_invariant(&id, statement, args.invariant_scope.as_deref()))
        .collect::<rusqlite::Result<Vec<_>>>()?;

    emit(&RecordResult {
        id,
        staged: true,
        declared_invariants,
    });
    Ok(())
}

/// Resolve the rationale: the sentinel `-` reads it from stdin (so agents can
/// pipe long prose without shell quoting); anything else is used verbatim. A
/// blank rationale is rejected — a rationale is required (§7.3).
fn resolve_rationale(arg: &str) -> Result<String, AppError> {
    let text = if arg == "-" {
        use std::io::Read;
        let mut buf = String::new();
        std::io::stdin().read_to_string(&mut buf)?;
        buf.trim_end().to_string()
    } else {
        arg.to_string()
    };
    if text.trim().is_empty() {
        return Err(AppError::new(
            "empty_rationale",
            "rationale must not be empty",
        ));
    }
    Ok(text)
}

/// Files changed in the working tree — staged, unstaged, and untracked — via
/// `git status --porcelain`, as workspace-relative anchor paths. Best-effort:
/// empty outside a git repo or on error.
///
/// Porcelain paths are reported relative to the repository root, so `git` is run
/// from the toplevel and its output is joined back onto it before normalizing.
/// That keeps the result independent of both the invocation directory and the
/// `status.relativePaths` setting. Note the git root need not be the dlog root —
/// `.dlog/` may sit above or below it.
fn git_changed_files(workspace: &Workspace, cwd: &std::path::Path) -> Vec<String> {
    let Some(toplevel) = git_toplevel() else {
        return Vec::new();
    };
    let Ok(output) = std::process::Command::new("git")
        .arg("-C")
        .arg(&toplevel)
        .args(["status", "--porcelain"])
        .output()
    else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    parse_porcelain(&String::from_utf8_lossy(&output.stdout))
        .iter()
        .map(|p| workspace.relativize(cwd, &toplevel.join(p).to_string_lossy()))
        // The store lives inside the tree it records, so git reports it as
        // changed on every invocation. A decision anchored to its own log is
        // noise.
        .filter(|p| !is_store_path(p))
        .collect()
}

/// Whether a workspace-relative path is the dlog store directory or inside it.
fn is_store_path(path: &str) -> bool {
    path == DLOG_DIR || path.starts_with(&format!("{DLOG_DIR}/"))
}

/// The git repository root (`git rev-parse --show-toplevel`), or `None` outside
/// a repo / when git is unavailable.
fn git_toplevel() -> Option<std::path::PathBuf> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let path = String::from_utf8(output.stdout).ok()?.trim().to_string();
    (!path.is_empty()).then(|| std::path::PathBuf::from(path))
}

/// Parse `git status --porcelain` into changed paths. Drops the 2-char status +
/// space prefix, resolves renames (`R  old -> new` → `new`), and unquotes paths.
fn parse_porcelain(text: &str) -> Vec<String> {
    let mut files = Vec::new();
    for line in text.lines() {
        if line.len() < 4 {
            continue;
        }
        let rest = &line[3..];
        let path = rest
            .rsplit(" -> ")
            .next()
            .unwrap_or(rest)
            .trim()
            .trim_matches('"');
        if !path.is_empty() {
            files.push(path.to_string());
        }
    }
    files
}

/// Fill an anchor's `symbol_path` / `node_kind` / `structural_hash` from the
/// enclosing definition (§10.2), when the anchor names a readable file in a
/// supported language with a line. Unsupported languages, unreadable paths, or
/// lines not inside a definition are left as file-level anchors (§10.5).
fn enrich_anchor(anchor: &mut Anchor, workspace: &Workspace) {
    let Some((line, _)) = anchor.line_span else {
        return;
    };
    let Some(lang) = crate::anchor::language_for_path(&anchor.file) else {
        return;
    };
    // `anchor.file` is workspace-relative; read through the root, not the cwd.
    let Ok(source) = std::fs::read_to_string(workspace.resolve_path(&anchor.file)) else {
        return;
    };
    if let Some(def) = crate::anchor::definition_at_line(&source, line, lang) {
        anchor.symbol_path = Some(def.symbol_path);
        anchor.node_kind = Some(def.node_kind);
        anchor.structural_hash = Some(def.structural_hash);
        anchor.line_span = Some(def.line_span);
    }
}

/// Parse an anchor spec: `FILE`, `FILE:LINE`, or `FILE:START-END`. The trailing
/// `:...` is only treated as a line span when it parses as one; otherwise the
/// whole string is the path. The path is normalized to its workspace-relative
/// form. Symbol/structural fields start empty and are filled by [`enrich_anchor`]
/// when the source is available.
fn parse_anchor(spec: &str, workspace: &Workspace, cwd: &std::path::Path) -> Anchor {
    let (file, line_span) = match spec.rsplit_once(':') {
        Some((path, lines)) => match parse_line_spec(lines) {
            Some(span) => (path.to_string(), Some(span)),
            None => (spec.to_string(), None),
        },
        None => (spec.to_string(), None),
    };
    Anchor {
        line_span,
        ..file_anchor(workspace.relativize(cwd, &file))
    }
}

/// A bare file-level anchor on an already-normalized path (§10.5).
fn file_anchor(file: String) -> Anchor {
    Anchor {
        file,
        symbol_path: None,
        node_kind: None,
        structural_hash: None,
        line_span: None,
        recorded_at_sha: None,
    }
}

/// Parse a rejected alternative `"approach :: reason"`. Without `::`, the whole
/// string is the approach and the reason is empty.
fn parse_rejected(spec: &str) -> Rejected {
    match spec.split_once("::") {
        Some((approach, reason)) => Rejected {
            approach: approach.trim().to_string(),
            reason: reason.trim().to_string(),
        },
        None => Rejected {
            approach: spec.trim().to_string(),
            reason: String::new(),
        },
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::commands::RootSource;
    use crate::store::Store;

    /// A workspace rooted at `/proj`, so anchor normalization is exercised
    /// without depending on the process cwd.
    fn workspace() -> Workspace {
        Workspace::rooted(PathBuf::from("/proj"), RootSource::Dlog, None)
    }

    #[test]
    fn parse_anchor_plain_file() {
        let a = parse_anchor("src/lib.rs", &workspace(), Path::new("/proj"));
        assert_eq!(a.file, "src/lib.rs");
        assert!(a.line_span.is_none());
        assert!(a.symbol_path.is_none());
    }

    #[test]
    fn parse_anchor_single_line_and_range() {
        let (ws, cwd) = (workspace(), Path::new("/proj"));
        assert_eq!(
            parse_anchor("src/lib.rs:12", &ws, cwd).line_span,
            Some((12, 12))
        );
        assert_eq!(
            parse_anchor("src/lib.rs:10-45", &ws, cwd).line_span,
            Some((10, 45))
        );
    }

    #[test]
    fn parse_anchor_non_line_suffix_is_part_of_path() {
        // A trailing colon that isn't a line spec stays in the path.
        let a = parse_anchor("weird:name", &workspace(), Path::new("/proj"));
        assert_eq!(a.file, "weird:name");
        assert!(a.line_span.is_none());
    }

    #[test]
    fn parse_anchor_normalizes_against_the_workspace_root() {
        let ws = workspace();
        // Recorded from a subdirectory: stored as if recorded from the root.
        let a = parse_anchor("lib.rs:12", &ws, Path::new("/proj/src"));
        assert_eq!(a.file, "src/lib.rs");
        assert_eq!(a.line_span, Some((12, 12)));

        // An absolute path inside the root collapses to the same anchor.
        let a = parse_anchor("/proj/src/lib.rs", &ws, Path::new("/proj/src"));
        assert_eq!(a.file, "src/lib.rs");

        // Outside the workspace: kept absolute rather than silently rewritten.
        let a = parse_anchor("/etc/hosts", &ws, Path::new("/proj/src"));
        assert_eq!(a.file, "/etc/hosts");
    }

    #[test]
    fn parse_rejected_with_and_without_reason() {
        let r = parse_rejected("polling :: wasteful");
        assert_eq!(r.approach, "polling");
        assert_eq!(r.reason, "wasteful");

        let r = parse_rejected("just the approach");
        assert_eq!(r.approach, "just the approach");
        assert_eq!(r.reason, "");
    }

    #[test]
    fn store_paths_are_not_anchored() {
        // `.dlog/` shows up as untracked in every repo that uses dlog.
        assert!(is_store_path(".dlog"));
        assert!(is_store_path(".dlog/dlog.db"));
        assert!(!is_store_path(".dlogger/x.rs"));
        assert!(!is_store_path("src/.dlog.rs"));
    }

    #[test]
    fn parse_porcelain_extracts_changed_paths() {
        let text = " M src/a.rs\n?? src/new.rs\nA  src/added.rs\nR  src/old.rs -> src/renamed.rs\n";
        assert_eq!(
            parse_porcelain(text),
            vec!["src/a.rs", "src/new.rs", "src/added.rs", "src/renamed.rs"],
        );
        assert!(parse_porcelain("").is_empty());
    }

    fn temp_db() -> PathBuf {
        std::env::temp_dir().join(format!("dlog-record-{}.db", ulid::Ulid::new()))
    }

    fn args_with_db(db: &Path) -> RecordArgs {
        RecordArgs {
            rationale: "switch to exponential backoff for retries".into(),
            files: vec!["src/auth.rs:10-45".into()],
            changed: false,
            agent_role: "implementer".into(),
            agent_model: "claude-test".into(),
            agent_session: None,
            conversation_id: None,
            task_id: None,
            instruction: None,
            rejected: vec!["polling :: wasteful".into()],
            caused_by: vec![],
            supersedes: None,
            declares_invariant: vec!["tokens never logged".into()],
            invariant_scope: Some("src/auth".into()),
            db: Some(db.to_string_lossy().into_owned()),
        }
    }

    #[test]
    fn record_stages_decision_with_anchor_rejected_and_invariant() {
        let db = temp_db();
        run(args_with_db(&db)).expect("record should succeed");

        let store = Store::open(&db).unwrap();
        let hits = store.search("backoff").unwrap();
        assert_eq!(hits.len(), 1, "decision should be searchable");

        let d = store.get_decision(&hits[0]).unwrap().unwrap();
        assert!(d.staged, "freshly recorded decision is staged");
        assert!(d.binding.is_none());
        assert_eq!(d.anchors[0].file, "src/auth.rs");
        assert_eq!(d.anchors[0].line_span, Some((10, 45)));
        assert_eq!(d.rejected[0].approach, "polling");

        let invariants = store.live_invariants().unwrap();
        assert_eq!(invariants.len(), 1);
        assert_eq!(invariants[0].1, "tokens never logged");

        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn record_links_task_and_records_instruction() {
        let db = temp_db();
        let mut args = args_with_db(&db);
        args.task_id = Some("tsk_demo".into());
        args.instruction = Some("make the client resilient".into());
        run(args).expect("record with task should succeed");

        let store = Store::open(&db).unwrap();
        let id = &store.search("backoff").unwrap()[0];
        let d = store.get_decision(id).unwrap().unwrap();
        assert_eq!(d.task_id.as_deref(), Some("tsk_demo"));

        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn record_enriches_anchor_from_rust_source() {
        let dir = std::env::temp_dir().join(format!("dlog-src-{}", ulid::Ulid::new()));
        std::fs::create_dir_all(&dir).unwrap();
        let rs = dir.join("auth.rs");
        std::fs::write(
            &rs,
            "impl AuthService {\n    fn authenticate(&self) -> bool {\n        true\n    }\n}\n",
        )
        .unwrap();

        let db = temp_db();
        let mut args = args_with_db(&db);
        // Line 3 (`true`) is inside AuthService::authenticate.
        args.files = vec![format!("{}:3", rs.display())];
        run(args).expect("record should succeed");

        let store = Store::open(&db).unwrap();
        let id = &store.search("backoff").unwrap()[0];
        let d = store.get_decision(id).unwrap().unwrap();
        let anchor = &d.anchors[0];
        assert_eq!(
            anchor.symbol_path.as_deref(),
            Some("AuthService::authenticate")
        );
        assert_eq!(anchor.node_kind.as_deref(), Some("method"));
        assert!(anchor.structural_hash.is_some());

        let _ = std::fs::remove_file(&db);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
