# ARD discovery assets

[ARD (Agentic Resource Discovery)](https://github.com/ards-project/ard-spec)
lets agents discover capabilities by fetching `/.well-known/ai-catalog.json`
from a host. This directory holds the tooling that keeps dlog's catalog valid.

- **`ai-catalog.schema.json`** — a **vendored, pinned** copy of the ARD
  `ai-catalog` schema (`specVersion` 1.0). ARD is new and fluid, so we validate
  against this local copy instead of fetching upstream at CI time. Source:
  `https://raw.githubusercontent.com/ards-project/ard-spec/main/spec/schemas/ai-catalog.schema.json`.
  To refresh, re-pull upstream, eyeball the diff, and bump `specVersion`
  handling in the catalog if the spec moved.
- **`validate_catalog.py`** — validates `docs/.well-known/ai-catalog.json`
  against the vendored schema plus a couple of project-specific sanity checks
  (pinned `specVersion`, expected entry identifier). Run:

  ```bash
  pip install jsonschema
  python3 scripts/ard/validate_catalog.py
  ```

  CI runs this on every push/PR via `.github/workflows/ard-validate.yml`.

## The catalog

`docs/.well-known/ai-catalog.json` advertises dlog as a single
`application/ai-skill` entry whose `url` points at the agent skill doc
(`templates/AGENTS.md`, which covers install + usage). It is served by GitHub
Pages from `docs/` (`.nojekyll` lets the `.well-known` dot-dir through).

**v1 hosting caveat:** project Pages serve under a subpath
(`https://nonokia.github.io/dlog/.well-known/ai-catalog.json`), not the domain
root that strict ARD discovery expects. Domain-anchored hosting (a user/org
Pages site or a custom domain at `/.well-known/...`) is a deliberate follow-up.
