//! Command-line surface (clap).
//!
//! Subcommands that aren't implemented yet are unit variants; the feature issue
//! that owns each one fleshes out its arguments. `record` (#5) is the first with
//! a real argument surface.

use clap::{Args, Parser, Subcommand, ValueEnum};

#[derive(Debug, Parser)]
#[command(
    name = "dlog",
    version,
    about = "Agent-first decision log that sits alongside Git"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Record a decision into staging (#5).
    // Boxed: RecordArgs is far larger than the other (unit) variants.
    Record(Box<RecordArgs>),
    /// Explain the decisions behind a file:line or symbol (#9).
    Why(WhyArgs),
    /// Show full decisions by id (#10).
    Show(ShowArgs),
    /// Seal staged decisions, binding them to a commit sha (#6).
    Bind(BindArgs),
    /// Run `git commit`, then seal staged decisions to the new commit (#26).
    Commit(CommitArgs),
    /// Report store state: unsealed staging, schema version (#10).
    Status(StatusArgs),
    /// Full-text search over recorded decisions (#10).
    Search(SearchArgs),
    /// List live declared invariants (#21).
    Invariants(InvariantsArgs),
    /// Install/uninstall the git post-commit auto-seal hook (#27).
    Hooks(HooksArgs),
}

impl Command {
    /// Stable command name, used in diagnostics and JSON output.
    pub fn name(&self) -> &'static str {
        match self {
            Command::Record(_) => "record",
            Command::Why(_) => "why",
            Command::Show(_) => "show",
            Command::Bind(_) => "bind",
            Command::Commit(_) => "commit",
            Command::Status(_) => "status",
            Command::Search(_) => "search",
            Command::Invariants(_) => "invariants",
            Command::Hooks(_) => "hooks",
        }
    }
}

/// Arguments for `dlog record` (design §7.3, §7.4).
///
/// Only `--rationale`, at least one `--file` anchor, and the agent identity are
/// required (kept minimal to avoid recording friction, §7.3). Agent identity
/// falls back to environment variables so an agent sets it once per session
/// rather than on every call.
///
/// Anchors here record only `file` and an optional line span; `symbol_path` /
/// `structural_hash` extraction is language-dependent and lands with the Rust
/// tree-sitter anchor work (#7). Until then every anchor is a file-level anchor
/// (§10.5).
#[derive(Debug, Args)]
pub struct RecordArgs {
    /// Why this decision was made (required).
    #[arg(long)]
    pub rationale: String,

    /// Anchor: FILE, FILE:LINE, or FILE:START-END. Repeatable; at least one.
    #[arg(long = "file", value_name = "FILE[:LINES]")]
    pub files: Vec<String>,

    /// Agent role, e.g. implementer or reviewer.
    #[arg(long = "agent-role", env = "DLOG_AGENT_ROLE")]
    pub agent_role: String,

    /// Agent model id.
    #[arg(long = "agent-model", env = "DLOG_AGENT_MODEL")]
    pub agent_model: String,

    /// Agent session id (optional).
    #[arg(long = "agent-session", env = "DLOG_AGENT_SESSION")]
    pub agent_session: Option<String>,

    /// Conversation id (Agent Trace compatible).
    #[arg(long = "conversation-id")]
    pub conversation_id: Option<String>,

    /// Task id this decision belongs to.
    #[arg(long = "task")]
    pub task_id: Option<String>,

    /// Original instruction, recorded on the task when it is first referenced.
    #[arg(long)]
    pub instruction: Option<String>,

    /// Rejected alternative as "approach :: reason". Repeatable.
    #[arg(long = "rejected", value_name = "APPROACH :: REASON")]
    pub rejected: Vec<String>,

    /// Decision id that caused this one (DAG edge). Repeatable.
    #[arg(long = "caused-by", value_name = "DECISION_ID")]
    pub caused_by: Vec<String>,

    /// Decision id this one supersedes.
    #[arg(long = "supersedes", value_name = "DECISION_ID")]
    pub supersedes: Option<String>,

    /// Invariant declared by this decision. Repeatable.
    #[arg(long = "declares-invariant", value_name = "STATEMENT")]
    pub declares_invariant: Vec<String>,

    /// Scope (path) applied to declared invariants.
    #[arg(long = "invariant-scope", value_name = "PATH")]
    pub invariant_scope: Option<String>,

    /// Store path. Defaults to $DLOG_DB, else `.dlog/dlog.db`.
    #[arg(long = "db", env = "DLOG_DB")]
    pub db: Option<String>,
}

/// Arguments for `dlog why` (design §9.1, §9.2).
///
/// `target` is a `file:line`, `file:start-end`, a bare file path, or a symbol
/// path (e.g. `AuthService::authenticate`). Results are the compact form
/// (two-stage retrieval, §9.1); the agent drills in with `dlog show <id>`.
#[derive(Debug, Args)]
pub struct WhyArgs {
    /// What to explain: file:line, file:start-end, a file path, or a symbol.
    #[arg(value_name = "FILE:LINE | SYMBOL")]
    pub target: String,

    /// Include superseded decisions (default: live decisions only, §9.1).
    #[arg(long = "include-superseded")]
    pub include_superseded: bool,

    /// Maximum results before truncating.
    #[arg(long, default_value_t = 20)]
    pub limit: usize,

    /// Store path. Defaults to $DLOG_DB, else `.dlog/dlog.db`.
    #[arg(long = "db", env = "DLOG_DB")]
    pub db: Option<String>,
}

/// Arguments for `dlog show` (design §9.1) — the drill-down after a compact
/// query. Returns the full record for each id (rejected, anchors, declared
/// invariants, binding).
#[derive(Debug, Args)]
pub struct ShowArgs {
    /// Decision id(s) to show in full.
    #[arg(value_name = "DECISION_ID", required = true)]
    pub ids: Vec<String>,

    /// Store path. Defaults to $DLOG_DB, else `.dlog/dlog.db`.
    #[arg(long = "db", env = "DLOG_DB")]
    pub db: Option<String>,
}

/// Arguments for `dlog status` (design §8.3, §9.2) — store-wide state.
#[derive(Debug, Args)]
pub struct StatusArgs {
    /// Store path. Defaults to $DLOG_DB, else `.dlog/dlog.db`.
    #[arg(long = "db", env = "DLOG_DB")]
    pub db: Option<String>,
}

/// Arguments for `dlog search` (design §9.2) — full-text search (SQLite FTS5),
/// returning the compact form.
#[derive(Debug, Args)]
pub struct SearchArgs {
    /// Full-text query over decision rationale/rejected prose.
    #[arg(long)]
    pub text: String,

    /// Include superseded decisions (default: live decisions only, §9.1).
    #[arg(long = "include-superseded")]
    pub include_superseded: bool,

    /// Maximum results before truncating.
    #[arg(long, default_value_t = 20)]
    pub limit: usize,

    /// Store path. Defaults to $DLOG_DB, else `.dlog/dlog.db`.
    #[arg(long = "db", env = "DLOG_DB")]
    pub db: Option<String>,
}

/// Arguments for `dlog invariants` (design §7.1, §9.2) — list live invariants,
/// optionally narrowed to a path scope.
#[derive(Debug, Args)]
pub struct InvariantsArgs {
    /// Narrow to invariants in effect at, or within, this path.
    #[arg(long, value_name = "PATH")]
    pub scope: Option<String>,

    /// Store path. Defaults to $DLOG_DB, else `.dlog/dlog.db`.
    #[arg(long = "db", env = "DLOG_DB")]
    pub db: Option<String>,
}

/// What `dlog hooks` should do to the post-commit hook.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum HookAction {
    /// Install the auto-seal post-commit hook.
    Install,
    /// Remove the managed post-commit hook block.
    Uninstall,
}

impl HookAction {
    pub fn as_str(self) -> &'static str {
        match self {
            HookAction::Install => "install",
            HookAction::Uninstall => "uninstall",
        }
    }
}

/// Arguments for `dlog hooks` (design §8.3; v0.2, #27) — manage the repo-side
/// post-commit hook that auto-seals staging after a plain `git commit`.
#[derive(Debug, Args)]
pub struct HooksArgs {
    /// Whether to install or uninstall the hook.
    #[arg(value_enum)]
    pub action: HookAction,
}

/// Arguments for `dlog bind` (design §8.2, §8.3).
///
/// Seals staged decisions. Provide a commit SHA for the code path, or `--none`
/// for the non-code path (investigation/review that led to no commit). By
/// default all staged decisions are sealed; `--decision` restricts the set.
#[derive(Debug, Args)]
pub struct BindArgs {
    /// Commit sha to bind staged decisions to. Omit when using --none.
    #[arg(value_name = "SHA")]
    pub sha: Option<String>,

    /// Seal with binding {type:none} instead of a commit. Excludes SHA.
    #[arg(long = "none", conflicts_with = "sha")]
    pub none: bool,

    /// Restrict to specific staged decision id(s). Default: all staged.
    #[arg(long = "decision", value_name = "DECISION_ID")]
    pub decisions: Vec<String>,

    /// Store path. Defaults to $DLOG_DB, else `.dlog/dlog.db`.
    #[arg(long = "db", env = "DLOG_DB")]
    pub db: Option<String>,
}

/// Arguments for `dlog commit` (design §8.3; v0.2, #26). Runs `git commit` and
/// seals staged decisions to the resulting commit. Put dlog flags before `--`;
/// everything after is passed through to git, e.g. `dlog commit -- -m "msg"`.
#[derive(Debug, Args)]
pub struct CommitArgs {
    /// Restrict the seal to specific staged decision id(s). Default: all staged.
    #[arg(long = "decision", value_name = "DECISION_ID")]
    pub decisions: Vec<String>,

    /// Store path. Defaults to $DLOG_DB, else `.dlog/dlog.db`.
    #[arg(long = "db", env = "DLOG_DB")]
    pub db: Option<String>,

    /// Arguments passed through to `git commit` (e.g. `-m "msg"`).
    #[arg(
        trailing_var_arg = true,
        allow_hyphen_values = true,
        value_name = "GIT_ARGS"
    )]
    pub git_args: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_argless_subcommands() {
        // status and bind have only optional args; the rest require operands.
        for arg in ["status", "bind"] {
            let cli = Cli::try_parse_from(["dlog", arg]).expect("subcommand should parse");
            assert_eq!(cli.command.name(), arg);
        }
    }

    #[test]
    fn show_requires_ids_and_search_requires_text() {
        assert!(Cli::try_parse_from(["dlog", "show"]).is_err());
        assert!(Cli::try_parse_from(["dlog", "search"]).is_err());
        let cli = Cli::try_parse_from(["dlog", "show", "dec_1", "dec_2"]).expect("show parses");
        match cli.command {
            Command::Show(args) => assert_eq!(args.ids, vec!["dec_1", "dec_2"]),
            _ => panic!("expected show"),
        }
    }

    #[test]
    fn commit_captures_passthrough_git_args() {
        let cli =
            Cli::try_parse_from(["dlog", "commit", "--", "-m", "msg"]).expect("commit parses");
        match cli.command {
            Command::Commit(args) => assert_eq!(args.git_args, vec!["-m", "msg"]),
            _ => panic!("expected commit"),
        }
    }

    #[test]
    fn why_requires_a_target() {
        assert!(Cli::try_parse_from(["dlog", "why"]).is_err());
        let cli = Cli::try_parse_from(["dlog", "why", "src/auth.rs:23"]).expect("why parses");
        match cli.command {
            Command::Why(args) => assert_eq!(args.target, "src/auth.rs:23"),
            _ => panic!("expected why"),
        }
    }

    #[test]
    fn record_requires_rationale_and_agent_identity() {
        // Missing required args -> usage error.
        assert!(Cli::try_parse_from(["dlog", "record"]).is_err());
    }

    #[test]
    fn record_parses_minimal_required_surface() {
        let cli = Cli::try_parse_from([
            "dlog",
            "record",
            "--rationale",
            "add retry",
            "--file",
            "src/auth.rs:10-45",
            "--agent-role",
            "implementer",
            "--agent-model",
            "claude-test",
        ])
        .expect("minimal record should parse");
        assert_eq!(cli.command.name(), "record");
        match cli.command {
            Command::Record(args) => {
                let args = *args;
                assert_eq!(args.rationale, "add retry");
                assert_eq!(args.files, vec!["src/auth.rs:10-45".to_string()]);
                assert_eq!(args.agent_role, "implementer");
            }
            _ => panic!("expected record"),
        }
    }

    #[test]
    fn rejects_unknown_subcommand() {
        assert!(Cli::try_parse_from(["dlog", "nope"]).is_err());
    }

    #[test]
    fn requires_a_subcommand() {
        assert!(Cli::try_parse_from(["dlog"]).is_err());
    }
}
