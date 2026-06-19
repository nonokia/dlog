# Design: Prebuilt Binaries + Homebrew (no crates.io)

## Build/release tooling

Use **`cargo-dist`** (the `dist` tool) to generate the release pipeline. It is
designed for exactly this case — distributing a Rust binary via GitHub Releases
without (or alongside) crates.io:

- `dist init` writes `.github/workflows/release.yml` and a `[workspace.metadata.dist]`
  (or `dist-workspace.toml`) config.
- On pushing a tag (`v*`), CI builds each target on its native runner (so the
  bundled C — SQLite, tree-sitter — compiles per platform), then uploads:
  binaries, `.tar.xz`/`.zip` archives, **SHA256SUMS**, a **shell installer**
  (`curl --proto '=https' -sSf <url>/install.sh | sh`), and **`cargo binstall`**
  metadata.
- Targets v1: `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`,
  `x86_64-apple-darwin`, `aarch64-apple-darwin`.

Alternative considered: a hand-rolled matrix workflow. Rejected for v1 — more
maintenance and we'd re-implement the installer/checksums that `dist` gives free.

## Install paths exposed

- **Script:** the `dist`-generated `install.sh` (per-release URL), pinned by tag.
- **`cargo install --git https://github.com/nonokia/dlog`** — works now; note it
  needs a C toolchain (cc) for the bundled SQLite/tree-sitter.
- **`cargo binstall dlog --git https://github.com/nonokia/dlog`** — prebuilt, via
  the metadata `dist` emits.
- **Homebrew:** a tap repo `nonokia/homebrew-tap` with `Formula/dlog.rb`. `dist`
  can emit and push the formula (`publish-jobs = ["homebrew"]`), or we maintain it
  by hand pointing at the release tarball + sha256. `brew install nonokia/tap/dlog`.

## Versioning

`Cargo.toml` `version` drives the tag. v1: cut `v0.2.0` (matches shipped v0.2).
Each release is a git tag; the workflow does the rest. Add release metadata to
`Cargo.toml` (`keywords`, `categories`, `readme`) for completeness even though we
don't publish to a registry.

## Risks / open questions

- **C toolchain in CI:** bundled SQLite + tree-sitter need `cc` per target;
  native runners cover linux/macOS. aarch64-linux may need cross or an arm runner
  — `dist` handles this (cross or arm runners); verify on first tag.
- **macOS Gatekeeper:** unsigned binaries warn on first run; document the
  `xattr -d com.apple.quarantine` / right-click-open step, or add signing later.
- **Homebrew tap location:** needs a separate `nonokia/homebrew-tap` repo
  (decision: create it, or fold the formula into this repo's release notes).
- This change's apply is **verified in CI on a tag**, not locally (cross-platform
  builds + Release upload can't run in the dev sandbox).
