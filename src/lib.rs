//! dlog — an agent-first decision log that sits alongside Git.
//!
//! v0.1 skeleton. This crate wires up the clap dispatch table and the JSON I/O
//! contract (design §6.1, §9.3); every subcommand is currently a stub that
//! reports `not_implemented`. The feature issues fill them in incrementally.
//! Logic lives in the library (not `main`) so it can be unit-tested.

pub mod cli;
pub mod model;
pub mod output;
pub mod store;

use clap::Parser;

use cli::Cli;
use output::{EXIT_ERROR, ErrorEnvelope, emit};

/// Parse argv, dispatch to the matching command, and return a process exit code.
/// Usage errors are handled by clap (exit code 2) before control reaches here.
pub fn run() -> i32 {
    let cli = Cli::parse();

    // Every command is a stub in the skeleton, so report not-implemented
    // uniformly. As each command lands, replace this with a real dispatch arm.
    let env = ErrorEnvelope::new(
        "not_implemented",
        format!(
            "`dlog {}` is not implemented yet (tracked in #{})",
            cli.command.name(),
            cli.command.tracking_issue()
        ),
    );
    emit(&env);
    EXIT_ERROR
}
