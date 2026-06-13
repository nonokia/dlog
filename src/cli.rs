//! Command-line surface (clap).
//!
//! Each subcommand is a stub in this skeleton; the feature issue that owns it
//! (referenced below) fleshes out the arguments and behaviour. The skeleton's
//! job is the dispatch table and the JSON contract, not the per-command flags —
//! those are designed alongside each command so they aren't pre-empted here.

use clap::{Parser, Subcommand};

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
    Record,
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
            Command::Record => "record",
            Command::Why => "why",
            Command::Show => "show",
            Command::Bind => "bind",
            Command::Status => "status",
            Command::Search => "search",
        }
    }

    /// Issue tracking this command's implementation.
    pub fn tracking_issue(&self) -> u32 {
        match self {
            Command::Record => 5,
            Command::Why => 9,
            Command::Show => 10,
            Command::Bind => 6,
            Command::Status => 10,
            Command::Search => 10,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_each_subcommand() {
        let cases = ["record", "why", "show", "bind", "status", "search"];
        for arg in cases {
            let cli = Cli::try_parse_from(["dlog", arg]).expect("subcommand should parse");
            assert_eq!(cli.command.name(), arg);
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
