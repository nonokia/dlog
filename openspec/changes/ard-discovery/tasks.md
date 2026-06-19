# Tasks: ARD Discovery for dlog

> Sequence after (or with) the `distribution` change — the skill links to the
> install methods it defines.

- [ ] **Task 1 — GitHub Pages site that serves `.well-known`**

  Add a Pages source (`docs/`) with `.nojekyll` so the dotted directory is
  served. Enable Pages for the repo (Settings → Pages → `docs/`).

  Touch: `docs/.nojekyll` (and Pages settings).

  Verify: after enabling, `https://nonokia.github.io/dlog/.well-known/ai-catalog.json`
  returns the manifest (200, `application/json`).

- [ ] **Task 2 — Author the ARD catalog**

  Create `docs/.well-known/ai-catalog.json` with the single dlog `ai-skill`
  entry (identifier `urn:ai:github.com:nonokia:dlog`, `representativeQueries`,
  `tags`, `version`, `url` → the skill doc). Mirror `version` to the crate
  version.

  Touch: `docs/.well-known/ai-catalog.json`.

  Verify: valid JSON; fields match the ARD spec §4.

- [ ] **Task 3 — Skill doc carries install + usage**

  Add an "Install" section to `templates/AGENTS.md` (the `url` target),
  referencing the `distribution` install methods (curl script / brew / `cargo
  install --git`), so an agent goes find → install → use. (Or add a dedicated
  skill file and point `url` at it.)

  Touch: `templates/AGENTS.md`.

  Verify: the linked doc renders and contains install + usage.

- [ ] **Task 4 — Validate against the ARD schema**

  Fetch the ARD JSON Schema (`ards-project/ard-spec` `spec/schemas/`) and validate
  `ai-catalog.json` against it (a script or CI step). Fix any non-conformance.

  Touch: a small validation script / CI step (e.g. `scripts/validate-ard.*`).

  Verify: validation passes against the pinned `specVersion`.

- [ ] **Task 5 — Discovery hints + docs (optional, non-blocking)**

  Add an HTML `<link rel="ai-catalog" href="/.well-known/ai-catalog.json">` to the
  Pages index and a note in README about ARD discoverability. Optionally submit the
  catalog to a discovery service (Hugging Face Discover / GitHub Agent Finder).

  Touch: `docs/index.html`, `README.md`.

  Verify: catalog reachable and (if submitted) appears in a discovery search.
