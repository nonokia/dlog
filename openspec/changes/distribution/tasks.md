# Tasks: Prebuilt Binaries + Homebrew (no crates.io)

- [ ] **Task 1 — Release metadata in Cargo.toml**

  Add `keywords`, `categories`, and `readme = "README.md"` to `[package]`; confirm
  `description`, `license`, `repository`, and a top-level `LICENSE` file exist.
  Do NOT add a crates.io publish step.

  Touch: `Cargo.toml`.

  Verify: `cargo build` still green; `cargo package --list` shows README + LICENSE.

- [ ] **Task 2 — Release pipeline via cargo-dist**

  Run `dist init` (or hand-write equivalent): produce
  `.github/workflows/release.yml` + dist config targeting
  `{x86_64,aarch64}-linux-gnu` and `{x86_64,aarch64}-apple-darwin`, emitting
  archives, SHA256SUMS, a shell installer, and `cargo binstall` metadata. Trigger
  on tag `v*`.

  Touch: `.github/workflows/release.yml`, `dist-workspace.toml` (or
  `[workspace.metadata.dist]` in `Cargo.toml`).

  Verify: `dist plan`/`dist build` locally for the host target produces a `dlog`
  binary that runs `dlog --version`. (Full multi-target build verified in CI on
  the first tag.)

- [ ] **Task 3 — Homebrew tap**

  Enable cargo-dist's `homebrew` publish job (or add a `Formula/dlog.rb` in a
  `nonokia/homebrew-tap` repo) referencing the release artifacts. `brew install
  nonokia/tap/dlog` installs the `dlog` binary.

  Touch: dist config (`publish-jobs`) and/or the tap repo formula.

  Verify: formula `brew audit`/`brew install` from the tap once a release exists.

- [ ] **Task 4 — README install section + AGENTS note**

  Add an "Install" section to `README.md`: the `curl … | sh` script, `brew
  install nonokia/tap/dlog`, `cargo install --git …` (note the C-toolchain
  requirement), and "build from source" as the fallback. Mention install briefly
  in `templates/AGENTS.md` setup.

  Touch: `README.md`, `templates/AGENTS.md`.

  Verify: links/commands are correct for the chosen tag; `cargo fmt --all --
  --check` / `clippy -D warnings` / `test` green (no code change).

- [ ] **Task 5 — Cut v0.2.0**

  Tag `v0.2.0` to exercise the pipeline; confirm the Release has binaries +
  checksums + installer for all targets and that the install script fetches a
  runnable `dlog`.

  Verify: download a built artifact, `dlog --version` prints the version.
