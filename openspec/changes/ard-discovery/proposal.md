# Proposal: Make dlog Discoverable via ARD (Agentic Resource Discovery)

## What

Publish dlog as a discoverable agent resource under the **ARD specification**
(Agentic Resource Discovery — Google et al., 2026-06) so an agent can *search*
for it, then learn how to install and use it — without it being pre-configured.

Concretely: serve a domain-anchored **`/.well-known/ai-catalog.json`** (via the
repo's GitHub Pages) containing one ARD entry for dlog, typed as an
**`application/ai-skill`**, whose `url` points at a **skill document that
includes install + usage**. That gives the chain: ARD search → find dlog's skill
→ install (per the `distribution` change) → drive it via the CLI.

## Why

dlog is agent-first by design (§6.1): it's meant to be driven by coding agents,
and discoverability is the missing front door. ARD exists precisely to move tool
selection out of the LLM's context window into a search service, so agents can
find capabilities they weren't told about. dlog already ships an agent skill
(`templates/AGENTS.md`); ARD is the standard way to make that skill findable.
This pairs with the `distribution` change: ARD is the *discovery* layer,
prebuilt binaries are the *install* target the skill points to.

## Non-goals

- **No MCP server / A2A wrapper** in v1 — dlog is a CLI agents drive via bash;
  we represent it as an `ai-skill`, not `application/mcp-server+json`. An MCP
  bridge could be a later change if direct tool-calling is wanted.
- No custom domain in v1 — host on GitHub Pages (`*.github.io`). A dedicated
  domain (stronger ARD domain-anchoring/trust) is a follow-up.
- No automatic submission to external discovery services (Hugging Face Discover,
  GitHub Agent Finder) in v1 — a manual follow-up once the catalog is live.
- No change to `dlog`'s runtime behavior — this adds metadata + a published page.
