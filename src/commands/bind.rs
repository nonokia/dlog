//! `dlog bind` — seal staged decisions, stamping a binding (design §8.2, §8.3).
//!
//! Recording happens before a commit, so decisions accumulate in staging; this
//! command is the seal step that moves them into the immutable main log with an
//! explicit binding. The code path stamps `{type:commit, sha}`; the non-code
//! path (`--none`) stamps `{type:none}` for investigation/review that led to no
//! commit — the seal a subagent runs at task end so its decisions survive.

use serde::Serialize;

use crate::cli::BindArgs;
use crate::commands::{AppError, open_store};
use crate::model::Binding;
use crate::output::emit;

/// Success document for `dlog bind`.
#[derive(Debug, Serialize)]
struct BindResult {
    count: usize,
    sealed: Vec<String>,
    binding: Binding,
}

pub fn run(args: BindArgs) -> Result<(), AppError> {
    // `--none` and a SHA are mutually exclusive at the clap layer; here we only
    // have to reject the "neither" case.
    let binding = match (args.none, args.sha) {
        (true, _) => Binding::None,
        (false, Some(sha)) => Binding::Commit { sha },
        (false, None) => {
            return Err(AppError::new(
                "missing_binding",
                "bind requires a commit SHA or --none",
            ));
        }
    };

    let store = open_store(args.db)?;
    let only = if args.decisions.is_empty() {
        None
    } else {
        Some(args.decisions.as_slice())
    };

    let sealed = store.seal_staged(&binding, only)?;
    emit(&BindResult {
        count: sealed.len(),
        sealed,
        binding,
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::model::{Agent, Anchor, NewDecision};
    use crate::store::Store;

    fn temp_db() -> PathBuf {
        std::env::temp_dir().join(format!("dlog-bind-{}.db", ulid::Ulid::new()))
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

    fn bind_args(db: &Path) -> BindArgs {
        BindArgs {
            sha: None,
            none: false,
            decisions: vec![],
            db: Some(db.to_string_lossy().into_owned()),
        }
    }

    #[test]
    fn bind_seals_all_staged_to_commit() {
        let db = temp_db();
        let (a, b) = {
            let store = Store::open(&db).unwrap();
            (stage(&store, "one"), stage(&store, "two"))
        };

        let mut args = bind_args(&db);
        args.sha = Some("a3f9".into());
        run(args).unwrap();

        let store = Store::open(&db).unwrap();
        for id in [&a, &b] {
            let d = store.get_decision(id).unwrap().unwrap();
            assert!(!d.staged);
            assert_eq!(d.binding, Some(Binding::Commit { sha: "a3f9".into() }));
        }
        assert_eq!(store.status().unwrap().staging_count, 0);
        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn bind_none_seals_without_a_commit() {
        let db = temp_db();
        let id = {
            let store = Store::open(&db).unwrap();
            stage(&store, "investigation only")
        };

        let mut args = bind_args(&db);
        args.none = true;
        run(args).unwrap();

        let store = Store::open(&db).unwrap();
        let d = store.get_decision(&id).unwrap().unwrap();
        assert_eq!(d.binding, Some(Binding::None));
        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn bind_can_target_specific_decisions() {
        let db = temp_db();
        let (a, b) = {
            let store = Store::open(&db).unwrap();
            (stage(&store, "first"), stage(&store, "second"))
        };

        let mut args = bind_args(&db);
        args.sha = Some("c0ffee".into());
        args.decisions = vec![a.clone()];
        run(args).unwrap();

        let store = Store::open(&db).unwrap();
        assert!(!store.get_decision(&a).unwrap().unwrap().staged);
        assert!(
            store.get_decision(&b).unwrap().unwrap().staged,
            "b untouched"
        );
        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn bind_neither_sha_nor_none_errors() {
        let db = temp_db();
        let err = run(bind_args(&db)).unwrap_err();
        assert_eq!(err.code, "missing_binding");
        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn bind_empty_staging_is_a_no_op() {
        let db = temp_db();
        let mut args = bind_args(&db);
        args.none = true;
        run(args).unwrap(); // fresh store, nothing staged
        let store = Store::open(&db).unwrap();
        assert_eq!(store.status().unwrap().staging_count, 0);
        let _ = std::fs::remove_file(&db);
    }
}
