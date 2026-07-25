# Proposal: Workspace Root Discovery + `dlog init`

> Second OpenSpec change in this repo (design §12). Motivated by the question
> "can dlog record agent decisions without git?" — the answer is *almost*, and
> this change closes the gap that actually blocks it.

## What

Give dlog its own notion of a **workspace root**, so that every invocation lands
on the same store and the same path spelling regardless of which directory it is
run from — with or without git.

- **Root discovery**: walk upward from the current directory for a `.dlog/`
  directory; failing that, for a `.git` entry; failing that, use the current
  directory. The store default becomes `<root>/.dlog/dlog.db` instead of the
  CWD-relative `.dlog/dlog.db`.
- **Root-relative anchor paths**: paths given to `record`, `why`, and `context`
  are normalized (lexically) against the root before they are stored or matched,
  so `dlog record --file auth.rs` from `src/` records `src/auth.rs`.
- **`dlog init`**: create `.dlog/` in the current directory and initialize the
  store, so a project can declare its root explicitly instead of relying on
  `.git` being there.
- **`dlog status` reports the workspace**: `root`, `db`, and `root_source`, so an
  agent can see which store it is talking to.

## Why

The core of dlog is already git-independent: `record`, every query, `bind`, and
the whole query-time resolution path (§10) touch SQLite and the working tree
only — `recorded_at_sha` is nullable and never read back by a query. Only
`dlog commit` and `dlog hooks` genuinely require git, and §8.1 already scopes
git integration to "the moment of commit" alone.

What is *not* git-independent is the implicit assumption that somebody else
defines the project root. `resolve_db` defaults to the **CWD-relative** string
`.dlog/dlog.db` with no upward search, and anchor paths are stored verbatim
(matched by exact string and prefix `LIKE` in `decision_ids_by_file` /
`decision_ids_under_path`). Inside a git repo this happens to work because agents
run from the repo root. It fails the moment they don't:

- `cd src && dlog status` silently **creates a second store** at
  `src/.dlog/dlog.db`, splitting the log in two.
- A decision recorded as `auth.rs` from `src/` never matches `src/auth.rs`
  recorded from the root, so `dlog why` misses it.

Without git there is no fallback notion of a root at all, which is what makes
"dlog feels like a git accessory" true in practice. Making the root a first-class
dlog concept is the smallest change that makes the tool stand on its own, and it
fixes a real store-splitting bug for existing git users at the same time.

`dlog init` and the `status` workspace fields follow §9.1 principle 2: they
report state the agent cannot otherwise derive (which root, which store, whether
a nested store shadows an outer one), and offer no suggestions.

## Non-goals

- **No new `binding` type.** The `{type:"commit",sha}` / `{type:"none"}` enum is
  settled (§8.2, §11 item 3). Non-code sealing stays `dlog bind --none`, and this
  change does not reinterpret `none` as "no git" — `none` keeps its §8.2 meaning
  (a decision that led to no commit).
- **No task lifecycle.** `dlog task start/done` (§8.3's non-code seal trigger,
  listed as unimplemented in §13) is a separate change.
- **No generalization of `recorded_at_sha`** (e.g. a `--revision` flag for
  jj/hg/Sapling). It stays a best-effort git annotation.
- **No change to `dlog commit` / `dlog hooks`.** Requiring git is correct for the
  two git-integration commands.
- **No change to anchor resolution** (§10.3's two-axis matrix) or to the
  `resolution` enum.
- **No schema change and no store migration** — paths recorded from a repo root
  are already root-relative, so they normalize to the same string.
- No `.gitignore` management for `.dlog/`, and no changes to `.github/` or CI.
