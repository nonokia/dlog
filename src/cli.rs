//! Command-line surface (clap).
//!
//! Subcommands that aren't implemented yet are unit variants; the feature issue
//! that owns each one fleshes out its arguments. `record` (#5) is the first with
//! a real argument surface.

use clap::{Args, Parser, Subcommand};

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
    Why,
    /// Show full decisions by id (#10).
    Show,
    /// Seal staged decisions, binding them to a commit sha (#6).
    Bind,
    /// Report store state: unsealed staging, schema version (#10).
    Status,
    /// Full-text search over recorded decisions (#10).
    Search,
}

impl Command {
    /// Stable command name, used in diagnostics and JSON output.
    pub fn name(&self) -> &'static str {
        match self {
            Command::Record(_) => "record",
            Command::Why => "why",
            Command::Show => "show",
            Command::Bind => "bind",
            Command::Status => "status",
            Command::Search => "search",
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_argless_subcommands() {
        for arg in ["why", "show", "bind", "status", "search"] {
            let cli = Cli::try_parse_from(["dlog", arg]).expect("subcommand should parse");
            assert_eq!(cli.command.name(), arg);
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
