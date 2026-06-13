//! dlog — an agent-first decision log that sits alongside Git.
//!
//! This crate wires up the clap dispatch table and the JSON I/O contract
//! (design §6.1, §9.3) and routes each subcommand to its handler. Commands that
//! aren't implemented yet return a `not_implemented` error. Logic lives in the
//! library (not `main`) so it can be unit-tested.

pub mod cli;
pub mod commands;
pub mod model;
pub mod output;
pub mod store;

use clap::Parser;

use cli::{Cli, Command};
use commands::AppError;
use output::{EXIT_ERROR, EXIT_OK, emit};

/// Parse argv, dispatch to the matching command, and return a process exit code.
/// Usage errors are handled by clap (exit code 2) before control reaches here.
pub fn run() -> i32 {
    let cli = Cli::parse();

    let result = match cli.command {
        Command::Record(args) => commands::record::run(*args),
        Command::Why => Err(AppError::not_implemented("why", 9)),
        Command::Show => Err(AppError::not_implemented("show", 10)),
        Command::Bind => Err(AppError::not_implemented("bind", 6)),
        Command::Status => Err(AppError::not_implemented("status", 10)),
        Command::Search => Err(AppError::not_implemented("search", 10)),
    };

    match result {
        Ok(()) => EXIT_OK,
        Err(e) => {
            emit(&e.into_envelope());
            EXIT_ERROR
        }
    }
}
