//! dlog binary entry point.
//!
//! All logic lives in the `dlog` library crate so it can be unit-tested; `main`
//! only maps the run result to a process exit code.

use std::process::ExitCode;

fn main() -> ExitCode {
    ExitCode::from(dlog::run() as u8)
}
