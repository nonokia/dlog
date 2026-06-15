//! dlog — an agent-first decision log that sits alongside Git.
//!
//! This crate wires up the clap dispatch table and the JSON I/O contract
//! (design §6.1, §9.3) and routes each subcommand to its handler. Commands that
//! aren't implemented yet return a `not_implemented` error. Logic lives in the
//! library (not `main`) so it can be unit-tested.

pub mod anchor;
pub mod cli;
pub mod commands;
pub mod model;
pub mod output;
pub mod resolve;
pub mod store;

use clap::Parser;

use cli::{Cli, Command};
use output::{EXIT_ERROR, EXIT_OK, emit};

/// Parse argv, dispatch to the matching command, and return a process exit code.
/// Usage errors are handled by clap (exit code 2) before control reaches here.
pub fn run() -> i32 {
    let cli = Cli::parse();

    let result = match cli.command {
        Command::Record(args) => commands::record::run(*args),
        Command::Why(args) => commands::why::run(args),
        Command::Show(args) => commands::show::run(args),
        Command::Bind(args) => commands::bind::run(args),
        Command::Commit(args) => commands::commit::run(args),
        Command::Status(args) => commands::status::run(args),
        Command::Search(args) => commands::search::run(args),
        Command::Invariants(args) => commands::invariants::run(args),
        Command::Hooks(args) => commands::hooks::run(args),
        Command::Context(args) => commands::context::run(args),
        Command::Trace(args) => commands::trace::run(args),
    };

    match result {
        Ok(()) => EXIT_OK,
        Err(e) => {
            emit(&e.into_envelope());
            EXIT_ERROR
        }
    }
}
