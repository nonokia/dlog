# Design: ARD Discovery for dlog

## ARD recap (what we must produce)

ARD (spec: ards-project/ard-spec; `agenticresourcediscovery.org/spec`) is
federated and domain-anchored. A provider serves, at minimum, a JSON manifest at
**`https://{domain}/.well-known/ai-catalog.json`** (other discovery hints: a
robots.txt Agentmap directive, an HTML `<link rel="ai-catalog">`, DNS SVCB).

Manifest shape (spec §4):
- root: `specVersion` ("1.0"), `host` (displayName, identifier, documentationUrl,
  …), `entries[]`.
- entry (required): `identifier` — URN `urn:ai:<publisher>:<namespace>:<name>`;
  `displayName`; `type` — an IANA media type. Exactly one of `url` (remote
  reference) or `data` (embedded). Optional: `description`, `tags`,
  `capabilities`, `representativeQueries`, `version`, `updatedAt`, `metadata`.
- The spec is artifact-agnostic; plain CLIs have no dedicated type, so we use
  **`application/ai-skill`** and point `url` at a skill doc.

## Hosting

Serve the catalog from the repo's **GitHub Pages**
(`https://nonokia.github.io/dlog/.well-known/ai-catalog.json`):

- Add a `docs/` Pages source (or `gh-pages`) containing `.well-known/ai-catalog.json`.
- **Add `.nojekyll`** so Pages serves the dotted `.well-known/` directory (Jekyll
  ignores dotfiles by default).
- Trust note: `github.io` is a shared host, so domain anchoring is weak (per-repo
  path, not a dedicated domain). Acceptable for v1; a custom domain is the
  follow-up for stronger provenance.

## The catalog entry

```jsonc
{
  "specVersion": "1.0",
  "host": {
    "displayName": "dlog",
    "documentationUrl": "https://github.com/nonokia/dlog"
  },
  "entries": [
    {
      "identifier": "urn:ai:github.com:nonokia:dlog",
      "displayName": "dlog — agent-first decision log",
      "type": "application/ai-skill",
      "url": "https://raw.githubusercontent.com/nonokia/dlog/main/templates/AGENTS.md",
      "description": "Record and query the decisions behind code (rationale, rejected alternatives, invariants) alongside Git, anchored to AST nodes. A CLI with JSON in/out, driven by agents.",
      "tags": ["decision-log", "git", "code-archaeology", "rust", "cli", "agent"],
      "representativeQueries": [
        "why is this code written this way?",
        "record the decision behind this change before committing",
        "what decisions and invariants apply to src/auth?",
        "trace what caused this decision"
      ],
      "version": "0.2.0"
    }
  ]
}
```

`url` points at the skill artifact. We keep `templates/AGENTS.md` as the canonical
skill, but **add an "Install" section at its top** (from the `distribution`
change) so a discovering agent can go straight from "found it" to "installed it"
to "using it". (If a structured `ai-skill` JSON schema is required by consumers,
add a sibling JSON descriptor later; markdown is acceptable as the referenced
artifact today.)

## Validation

Validate `ai-catalog.json` against ARD's published JSON Schema
(`ards-project/ard-spec` `spec/schemas/`) in CI or a one-off check, so the
manifest stays conformant as the (young, evolving) spec changes.

## Dependencies / risks

- **Depends on `distribution`** for the install instructions the skill links to;
  sequence ard-discovery after (or alongside) it.
- ARD is brand new and may change (well-known path, type names, schema). Pin to a
  `specVersion` and re-validate; keep the manifest small.
- `.well-known` on Pages requires `.nojekyll`; verify it actually serves.
