# Design: Workspace Root Discovery + `dlog init`

## Where it plugs in

Every command reaches the store through `commands/mod.rs::open_store(db)`, which
today is `resolve_db` (`--db` / `$DLOG_DB`, else the CWD-relative
`.dlog/dlog.db`) plus `create_dir_all` plus `Store::open`. That single funnel is
replaced by a `Workspace` value, so root discovery and path normalization land in
one place and every command picks them up.

Three commands additionally take a path *from the caller* and must normalize it:
`record` (`--file`, `--changed`), `why` (the `file:line` / file target), and
`context` (`<path>`). `show`, `search`, `invariants`, `trace`, `bind`, `commit`,
and `hooks` only need the store, so for them the change is mechanical.

## Root discovery

```rust
pub(crate) enum RootSource { Dlog, Git, Cwd }

fn discover_root(from: &Path) -> (PathBuf, RootSource)
```

Walk `from` and its ancestors, in order:

1. first directory containing a `.dlog` **directory** → `RootSource::Dlog`
2. else first directory containing a `.git` entry, file **or** directory (a
   worktree/submodule `.git` is a file) → `RootSource::Git`
3. else `from` itself → `RootSource::Cwd`

`.dlog` is checked before `.git` on purpose: it makes the root a property dlog
itself declares, so worktrees, submodules, and monorepo subprojects resolve
unambiguously and a non-git project is not a special case. The `.git` tier is
pure convenience for the common "I never ran `dlog init`" path, and it keeps
today's behavior for existing repos.

Both passes run over the full ancestor chain in one loop (checking `.dlog` first
at each level would make a nested `.git` beat an outer `.dlog`, which inverts the
intended precedence).

## `Workspace`

```rust
pub(crate) struct Workspace {
    pub root: PathBuf,          // absolute
    pub db: PathBuf,
    pub root_source: RootSource,
}

impl Workspace {
    pub fn discover(db_arg: Option<String>) -> Result<Self, AppError>;
    pub fn open(&self) -> Result<Store, AppError>;
    pub fn relativize(&self, cwd: &Path, path: &str) -> String;
    pub fn resolve_path(&self, rel: &str) -> PathBuf;
}
```

- `discover` reads `std::env::current_dir()`, runs `discover_root`, and sets
  `db` to `db_arg` when given (`--db` / `$DLOG_DB` keep their exact current
  meaning: an explicit store location) and `<root>/.dlog/dlog.db` otherwise.
  Root discovery still runs when `--db` is given, because path normalization
  needs a root either way.
- `open` is today's `open_store` body: `create_dir_all` on the parent, then
  `Store::open`.
- `resolve_path` is `root.join(rel)`, used wherever a file is actually read
  (tree-sitter enrichment in `record`, the `file:line` lookup in `why`).

## Lexical path normalization

`relativize` is **lexical** — it never calls `canonicalize` or touches the
filesystem. Anchors must work for files that do not exist (a decision about a
file you are about to write), and the existing tests anchor to paths like
`src/auth.rs` that are not on disk.

1. If `path` is relative, join it onto `cwd`.
2. Fold the component list: drop `.`, pop on `..` (a `..` that would escape the
   prefix is kept literally, which can only happen for paths outside the root).
3. If the result is under `root`, strip that prefix; otherwise keep it absolute —
   a decision may legitimately anchor outside the workspace, and silently
   rewriting it would be worse than an unusual-looking path.
4. Render with `/` separators so the stored spelling is platform-independent
   (path components are re-joined rather than the string being rewritten).

`cwd` is a parameter rather than being read inside the function so the normalizer
is a pure function: `std::env::set_current_dir` is process-global and would make
parallel `cargo test` flaky. Only the CLI entry points pass the real cwd.

Note the `.` case: `dlog context .` from the root normalizes to the root itself,
which yields an empty relative path. That is spelled `"."` so
`decision_ids_under_path` keeps working — it is special-cased to mean "the whole
workspace", matching every anchor.

## `dlog init`

New `commands/init.rs`, `InitArgs` in `cli.rs`, a `Command::Init` arm in
`cli.rs::name()` and the `lib.rs` dispatch table.

It creates `.dlog/` **in the current directory** (not the discovered root — the
point of `init` is to declare a new root) and opens the store so the schema is
laid down; migration is idempotent DDL replay, so re-running is safe.

```json
{"root":"/abs/proj","db":"/abs/proj/.dlog/dlog.db","created":true,"shadows":"/abs/parent"}
```

- `created` is false when the store file already existed.
- `shadows` is emitted **only** when an ancestor directory already has a `.dlog/`,
  i.e. when this new root will shadow an outer one for anything run below it.
  That is a footgun worth surfacing, and §9.1 principle 2 says to surface it as
  state, not as a warning telling the agent what to do.

`--db` is accepted for symmetry with the other commands; when given, that path is
initialized instead.

## `dlog status`

The emitted document gains `root`, `db`, and `root_source` (`"dlog"` / `"git"` /
`"cwd"`) alongside the existing `staging_count` / `oldest_staged_ms` /
`schema_version`. `StoreStatus` in `store.rs` stays store-internal; `status.rs`
composes a wrapper struct that flattens it. Knowing *which* store answered is the
observability half of this change — without it, a split store is invisible.

## `--changed` and git

`--changed` is inherently git-shaped (it asks git what changed) and stays that
way, but its output must be normalized like any other path. `git status
--porcelain` prints paths relative to the repository root, and running it from a
subdirectory with `status.relativePaths` set can change that; rather than depend
on either, the implementation resolves `git rev-parse --show-toplevel` first,
runs `git -C <toplevel> status --porcelain`, and treats the results as
toplevel-relative before handing them to `relativize`. Outside a repo it stays
best-effort and yields nothing, exactly as today.

Paths at or under `.dlog/` are dropped: the store now reliably sits inside the
tree it records, so git reports it as changed on every invocation, and a decision
anchored to its own log is noise.

## Compatibility

- No schema change; `SCHEMA_VERSION` stays at 1.
- Existing stores need no migration: paths recorded from a repo root are already
  root-relative and normalize to themselves.
- `--db` / `$DLOG_DB` semantics are unchanged, so the existing tests (which point
  at temp-file stores and use relative anchor paths) keep passing.
- The one behavior change for existing users is the intended fix: running from a
  subdirectory now joins the root's store instead of creating a new one.

## Alternatives considered

- **`git rev-parse --show-toplevel` for the root.** Rejected: it reintroduces the
  git dependency in the core path, costs a subprocess on every invocation, and
  gives no answer at all outside a repo.
- **Check `.git` before `.dlog`.** Rejected: in a monorepo or a submodule the
  outer `.git` would win over the project's own declared `.dlog`, and it keeps
  git as the definition of "project".
- **Canonicalizing paths.** Rejected: `canonicalize` fails on paths that do not
  exist yet and resolves symlinks, which would make the stored spelling depend on
  the machine.
- **Requiring `dlog init` before any command.** Rejected: recording friction is
  what §7.3 is written against; the `.git` and cwd tiers keep first use to a
  single command.
- **Storing absolute paths.** Rejected: the store would stop being portable
  between checkouts, and every existing anchor would need migrating.

## Risks / mitigations

- **An agent that deliberately kept per-directory stores** would now see them
  merge. Mitigation: `--db` / `$DLOG_DB` still pin a store explicitly, and
  `dlog init` in the subdirectory re-establishes a nested root (reported via
  `shadows`).
- **Anchors recorded before this change from a subdirectory** stay in their old
  spelling and will not match the normalized form. They are already unmatched
  today (that is the bug), so this is no worse; `dlog search --text` still finds
  them.
- **Lexical `..` handling differs from the filesystem when symlinks are
  involved.** Accepted: the alternative (canonicalize) breaks non-existent paths,
  which matters more.
