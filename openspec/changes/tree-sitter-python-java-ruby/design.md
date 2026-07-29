# Design: tree-sitter grammars for Python, Java, and Ruby

## The one structural change: `span_node`

`LangSupport` grows a single hook:

```rust
/// The node whose rows become the reported `line_span`, given the definition
/// node. Almost always the definition itself.
span_node: fn(Node) -> Node,
```

Everything except Python passes `|n| n`, and the hash is still taken over the
*definition* node, so this only widens the human snapshot (§10.2) — no existing
`structural_hash` changes and no stored anchor stops resolving as `exact`.

Python needs it because the grammar wraps a decorated definition:

```
decorated_definition
  decorator            @app.route("/")
  decorator            @cached
  function_definition  def index(): ...
```

The `function_definition` starts at the `def` line, so `dlog why app.py:1` — the
`@app.route` line, the line a reviewer is most likely to point at — would find no
enclosing definition and degrade to `file_fallback`. Reporting the wrapper's span
makes every line from the first decorator to the end of the body resolve to
`index`.

The decorators stay *out* of the hash. A decorator is a change to the definition,
so folding it into the hash would be defensible; but `definition_at_line` also
picks the innermost enclosing definition by span width, and a hash that covered
the wrapper while the span came from a different node would make the two hooks
disagree about what "the node" is. One node for identity, a possibly wider span
for the human snapshot, is the smaller rule.

## Python

| node | anchors as |
|---|---|
| `function_definition` | `method` inside a class, else `function` |
| `class_definition` | `class` |

Scope segments come from **both** `class_definition` and `function_definition`.
The other four languages don't nest functions inside functions, so `local` in
Rust's `fn outer() { fn local() {} }` stays unqualified — but in Python inner
functions are idiomatic (every decorator has a `wrapper`), and leaving them
unqualified would put several identically-named `wrapper` symbols in one file,
which is exactly the ambiguity `symbol_path` exists to avoid. So a nested def
reads `helper::local`, and `resets_method_scope` on `function_definition` keeps
it a *function* rather than inheriting the enclosing class's method context.

There is no module node: a `.py` file *is* the module, and the file is already the
anchor's `file`. `signature_shape` works unchanged — `function_definition` has a
`parameters` field and a `return_type` field, so arity and return annotation both
discriminate. `self` counts toward arity, which is fine: it is a real parameter
and a method that gains one is a different shape either way.

## Java

| node | anchors as |
|---|---|
| `method_declaration`, `constructor_declaration`, `compact_constructor_declaration` | `method` |
| `class_declaration`, `record_declaration` | `class` |
| `interface_declaration`, `annotation_type_declaration` | `interface` |
| `enum_declaration` | `enum` |

Java has no free functions, so `method_declaration` is unconditionally a method
and there is no method scope to open or reset — the `in_method` flag is unused,
like Go's. Constructors are anchored because "why is construction like this" is a
decision that belongs on the constructor, not on the class; the constructor's
`name` field is the type name, so `Client::Client` is the path (a record's
compact constructor likewise).

A record anchors as `class` rather than a kind of its own. `node_kind` is a
coarse label for the agent, not a language taxonomy, and every existing consumer
treats `class` as "the type this method hangs off".

`signature_shape` sees Java's arity (the `parameters` field is `formal_parameters`)
but not its return type, which Java's grammar names `type`, not `return_type`.
Reading `type` as "has a return type" was rejected: Go's `type_spec` also has a
`type` field, so it would flip that bit for every already-recorded Go struct and
interface and drift their stored anchors. The cost is one bit of discrimination on
Java methods only, and the return type's own tokens are still in the hashed
stream.

Comments list `line_comment`, `block_comment`, **and** `comment` — the grammar
split them in 0.23 and the third costs nothing to keep for older trees.

## Ruby

| node | anchors as |
|---|---|
| `method` | `method` inside a class/module, else `function` |
| `singleton_method` | `method` |
| `class` | `class` |
| `module` | `module` |

A top-level `def` is really a private method on `Object`, but labelling it
`function` matches how every other language here labels an unowned definition,
and it is the distinction an agent reading the output actually cares about.

**Singleton methods keep `self` in the path.** `def self.build` inside
`class Client` becomes `Client::self::build`, via `def_prefix` returning the
`object` field verbatim. Dropping `self` would read more naturally, but it would
collide `Client::build` the class method with `Client::build` the instance
method — two different definitions under one `symbol_path`, which resolution has
no way to tell apart. `class << self` gets the matching treatment through
`scope_segment` on `singleton_class` (its `value` is `self`), so both spellings
of a class method produce the same path. `def Foo.bar` at the top level yields
`Foo::bar`, which is the Go-receiver shape.

`class Foo::Bar` needs nothing special: the name is a `scope_resolution` whose
text is already `Foo::Bar`, the same separator the symbol path uses.

Identifier collapsing covers `identifier`, `constant`, `instance_variable`,
`class_variable`, `global_variable`, and `hash_key_symbol`. Instance variables
matter in particular — renaming `@url` to `@host` is an identifier rename and
must leave the hash invariant, and Ruby's grammar makes `@url` a single leaf
rather than a sigil plus an identifier.

## Extensions

`py` / `pyi`, `java`, `rb`. `.rbs` is a separate language (Ruby type signatures)
and is deliberately not mapped; `Rakefile` and `Gemfile` have no extension and
`language_for_path` is extension-only, so they keep file-level anchors.

## Alternatives considered

- **Fold decorators into the hashed node** (hash `decorated_definition`). It has
  no `parameters` field, so `signature_shape` would silently degrade to arity 0
  for every decorated function — losing the discriminator on exactly the
  definitions most likely to share a body shape. Rejected.
- **A `constructor` node_kind for Java.** New vocabulary in the output contract
  for a distinction agents can already read off the symbol path
  (`Client::Client`). §7.3 minimalism.
- **Drop `self` from Ruby singleton paths.** Prettier, ambiguous. Covered above.
- **Anchor Python nested functions unqualified**, matching Rust. Rejected for the
  `wrapper` collision, which is common enough in Python to be the normal case
  rather than the edge case.
- **Map `Rakefile` / `Gemfile` by filename.** `language_for_path` is
  extension-only by design; a filename table is a different (and larger)
  decision, and file-level anchors on build scripts are not obviously wrong.

## Risks / mitigations

- **Files that were file-level anchors become node anchors.** A decision recorded
  against `app.py` before this change stored no `symbol_path`, so it still
  resolves by file; only *new* records get nodes. Old and new coexist — the
  cascade in `resolve.rs` tries symbol first and falls through to file.
- **Grammar node-name drift across crate versions.** The classification tables
  name nodes as strings, so a renamed node silently stops matching. The
  extraction tests per language are what catch it; each of the three has one.
- **Three more C grammars in the build.** Compile time only; they are leaf
  dependencies with no shared transitive tree beyond `tree-sitter` itself.
