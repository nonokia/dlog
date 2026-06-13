//! dlog — an agent-first decision log that sits alongside Git.
//!
//! This is the v0.1 skeleton. The command surface (`record`, `why`, `show`,
//! `bind`, `status`, ...) is defined in `agent-first-vcs-design.md` and will be
//! filled in incrementally. For now `main` only reports the build version so
//! that CI has a binary to build and run.

fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

fn main() {
    println!("dlog {}", version());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_non_empty() {
        assert!(!version().is_empty());
    }
}
