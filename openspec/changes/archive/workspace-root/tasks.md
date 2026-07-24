# Tasks: Workspace Root Discovery + `dlog init`

- [x] **Task 1 — Root discovery + `Workspace` in the shared command layer**

  In `src/commands/mod.rs`: add `RootSource` (`Dlog` / `Git` / `Cwd`, serialized
  as `"dlog"` / `"git"` / `"cwd"`), the pure `discover_root(from: &Path) ->
  (PathBuf, RootSource)` (ancestor walk: `.dlog` dir, then `.git` file-or-dir,
  then `from`), and the `Workspace` struct with `discover` / `open` /
  `relativize(cwd, path)` / `resolve_path(rel)`. `relativize` is lexical (no
  `canonicalize`), folds `.` and `..`, strips the root prefix when the path is
  under it, keeps outside paths absolute, and joins with `/`; the root itself
  normalizes to `"."`. Remove `resolve_db`/`open_store` once callers move.

  Touch: `src/commands/mod.rs`.

  Verify: unit tests in the same module — `.dlog` beats a nearer `.git`, `.git`
  is used when no `.dlog` exists, cwd is the fallback, discovery works from a
  nested directory; `relativize` handles a subdirectory-relative path, an
  absolute path inside the root, a path with `..`, a path outside the root
  (stays absolute), and a non-existent path.

- [x] **Task 2 — `dlog init`**

  New `src/commands/init.rs` emitting `{root, db, created, shadows?}`; `InitArgs`
  (`--db`) and a `Command::Init` variant + `name()` arm in `src/cli.rs`; dispatch
  arm in `src/lib.rs`; module declaration in `src/commands/mod.rs`. Creates
  `.dlog/` in the current directory (not the discovered root) and opens the store
  so the schema is applied. `created` is false on re-run; `shadows` is emitted
  only when an ancestor already has a `.dlog/`.

  Touch: `src/commands/init.rs`, `src/cli.rs`, `src/lib.rs`,
  `src/commands/mod.rs`.

  Verify: unit tests — a fresh temp dir yields `created: true` and a second call
  `created: false`; a child of a dir with `.dlog/` reports `shadows`. Manually:
  `cd $(mktemp -d) && dlog init` prints the JSON and creates `.dlog/dlog.db`.

- [x] **Task 3 — Normalize paths in `record`**

  In `src/commands/record.rs`: build a `Workspace`, run every `--file` anchor
  path through `relativize`, and read source for `enrich_anchor` via
  `resolve_path` (pass the resolved path in rather than reading `anchor.file`).
  For `--changed`, resolve `git rev-parse --show-toplevel`, run
  `git -C <toplevel> status --porcelain`, join each porcelain path onto the
  toplevel, then `relativize`; drop paths at or under `.dlog/` (the store is
  untracked, so git reports it every time); keep the outside-a-repo behavior
  (empty, so a `--changed`-only invocation still reports `missing_anchor`).

  Touch: `src/commands/record.rs`.

  Verify: existing record tests stay green; add a test that an anchor given as an
  absolute path inside the root is stored root-relative, and that
  `parse_porcelain` output is joined against the toplevel.

- [x] **Task 4 — Normalize paths in `why` and `context`**

  In `src/commands/why.rs`, thread the `Workspace` and cwd into
  `build_query_node` / `node_for_file_line`: match the DB on the root-relative
  path, read the file from `resolve_path`. In `src/commands/context.rs`,
  normalize `args.path` before `decision_ids_under_path`, and let `"."` mean the
  whole workspace.

  Touch: `src/commands/why.rs`, `src/commands/context.rs`.

  Verify: existing tests green (they pass root-relative paths already); add a
  `context` test that `"."` returns every decision.

- [x] **Task 5 — Workspace fields on `status`, and the mechanical migration**

  In `src/commands/status.rs`, emit `root` / `db` / `root_source` alongside the
  flattened `StoreStatus`. Move the remaining `open_store` callers
  (`bind`, `commit`, `hooks`, `search`, `show`, `invariants`, `trace`) to
  `Workspace::discover(...)?.open()?`.

  Touch: `src/commands/status.rs`, `src/commands/{bind,commit,hooks,search,show,invariants,trace}.rs`.

  Verify: `cargo test --all-features`; `dlog status` JSON contains the three new
  fields.

- [x] **Task 6 — End-to-end check without git**

  In a temp directory with **no** `git init`: `dlog init`, record a decision
  anchored at `src/auth.rs`, then from `src/` run `status` (root points at the
  parent, `staging_count` is 1), `why src/auth.rs`, `context .`, and
  `bind --none`, and confirm no `src/.dlog/` was created. Re-run `status` /
  `why` from the root of this repo (which has git) to confirm no regression.

  Touch: none (manual verification).

  Verify: the sequence above behaves as described.

- [x] **Task 7 — Docs + gate**

  `templates/AGENTS.md`: mention `dlog init`, the store living at the workspace
  root, and that dlog works without git (sealing via `bind --none`).
  `README.md`: add `dlog init` to the command list and note the root discovery
  order. `agent-first-vcs-design.md` §13 ("意図的な改良"): record workspace root
  discovery as an implementation note — not a re-litigation of §8.

  Touch: `templates/AGENTS.md`, `README.md`, `agent-first-vcs-design.md`.

  Verify: `cargo fmt --all -- --check`, `RUSTFLAGS="-D warnings" cargo clippy
  --all-targets --all-features`, `cargo test --all-features` all green.
