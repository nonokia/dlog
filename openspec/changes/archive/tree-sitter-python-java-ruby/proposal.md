# Proposal: tree-sitter grammars for Python, Java, and Ruby

> dlog issue #62.

## What

Extend AST node anchoring (design §10) from **Rust / TypeScript·TSX / Go / PHP**
to **Python**, **Java**, and **Ruby**:

- `language_for_path` learns `.py` / `.pyi`, `.java`, and `.rb`.
- Three new `LangSupport` entries in `src/anchor.rs` — node classification,
  `symbol_path` nesting rules, identifier/comment kinds for the structural hash.
- One new hook on `LangSupport`, `span_node`, so a definition can report a
  `line_span` wider than the node it hashes. Python needs it: a decorated
  definition is wrapped in `decorated_definition`, and the decorator lines are
  part of the definition a human (or `dlog why file:line`) points at.

Everything else is untouched: the tree walk, the structural hash, resolution, and
every language-independent command (§10.5).

## Why

§10.5 splits the tool so that language support costs exactly one `LangSupport`
table entry, and §10.6 lists more grammars as the cheap follow-on work. Until
now every non-listed file degrades to `file_fallback` — correct, but it means a
decision recorded against a Python function is a decision about *the file*, and
it stops surviving the refactors that node anchoring exists to survive.

Python is the gap that hurts: it is the most common language in the agent work
dlog is built for, and it is also the one where the "anchor to a named
definition" model fits most directly. Java and Ruby come along because they are
the same shape of work — a classification table and a nesting rule — and doing
the three together is what forces the one structural change (`span_node`) to be
designed once rather than bolted on per language.

## Non-goals

- No change to the language-independent core: recording, staging, sealing,
  binding, and the query commands are untouched (§10.5).
- No change to the behavior of still-unsupported languages. `file_fallback` is
  the correct answer for them, not a bug to route around.
- No markup / config formats (YAML, Markdown, JSON). They have no named
  definition nodes; a file-level anchor is already the right granularity.
- No change to the existing four languages' `symbol_path` or `structural_hash`
  output — stored anchors must keep resolving as `exact`. In particular
  `signature_shape` is left alone.
- C#, Kotlin, and Swift stay unscoped (issue #62 defers them to demand).
