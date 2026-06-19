# Proposal: Installable Without Cloning (Prebuilt Binaries + Homebrew)

## What

Let people and agents install `dlog` without cloning and building the repo,
**without publishing to crates.io** (the `dlog` crate name is taken and we're
not releasing there). Provide:

- **Prebuilt binaries via GitHub Releases** for the common targets (Linux
  x86_64 + aarch64, macOS x86_64 + aarch64), attached to a tag, with checksums.
- A **one-line install script** (`curl … | sh`) that downloads the right
  binary for the host.
- **`cargo install --git`** documented for Rust users (works today, no registry).
- A **Homebrew tap** (`brew install nonokia/tap/dlog`).

The binary stays named `dlog`.

## Why

Today the only path is `git clone` + `cargo build`, which also requires a C
toolchain because `dlog` bundles SQLite and tree-sitter (C). That's a high bar —
especially for non-Rust users and for agents that want to adopt the tool on the
fly. Prebuilt binaries + a curl installer + Homebrew make "search → install →
use" a few seconds instead of a full Rust build, and are the install target the
ARD discovery work (separate change) will point agents at.

## Non-goals

- **No crates.io publish** (deferred; name taken, not releasing there).
- No Windows installer (MSI) in v1 — Windows binaries can come later.
- No Scoop / Nix / AUR in v1 (cheap to add once Releases exist).
- No macOS code-signing/notarization in v1 (document the Gatekeeper step).
- No change to `dlog`'s behavior — packaging only.
