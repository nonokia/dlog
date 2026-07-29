# Tasks: tree-sitter grammars for Python, Java, and Ruby

- [x] **Task 1 — Add the grammar crates**

  `tree-sitter-python` 0.25, `tree-sitter-java` 0.23, `tree-sitter-ruby` 0.23 —
  all compatible with the pinned `tree-sitter` 0.26.

  Touch: `Cargo.toml`, `Cargo.lock`.

  Verify: `cargo build` succeeds.

- [x] **Task 2 — `span_node` hook on `LangSupport`**

  Add `span_node: fn(Node) -> Node`, use it in `walk` for `line_span` only (the
  hash stays on the definition node), and pass `|n| n` for the five existing
  tables.

  Touch: `src/anchor.rs`.

  Verify: existing Rust/TS/Go/PHP tests unchanged and green — no stored hash
  moves.

- [x] **Task 3 — Python support**

  `PYTHON` table: `function_definition` → method/function, `class_definition` →
  class; scope segments from both class *and* function definitions (nested defs
  are idiomatic); `span_node` widens to `decorated_definition`.

  Touch: `src/anchor.rs`.

  Verify: `extracts_python_definitions`,
  `python_decorator_lines_belong_to_the_definition`,
  `python_definition_at_line_and_hash_invariance`.

- [x] **Task 4 — Java support**

  `JAVA` table: method / constructor / compact-constructor → method; class and
  record → class; interface and annotation type → interface; enum → enum. No
  method scope (Java has no free functions).

  Touch: `src/anchor.rs`.

  Verify: `extracts_java_definitions`,
  `java_definition_at_line_and_hash_invariance`.

- [x] **Task 5 — Ruby support**

  `RUBY` table: `method` → method/function by scope, `singleton_method` →
  method with the receiver as a path prefix (`Client::self::build`), `class` /
  `module` scopes, `singleton_class` contributing its `value` as a segment.
  Identifier collapsing covers instance/class/global variables.

  Touch: `src/anchor.rs`.

  Verify: `extracts_ruby_definitions`,
  `ruby_definition_at_line_and_hash_invariance`.

- [x] **Task 6 — Extensions + docs + gate**

  `language_for_path`: `py` / `pyi`, `java`, `rb`. Update the language lists in
  `CLAUDE.md` and `README.md` and the module doc comment in `src/anchor.rs`.

  Touch: `src/anchor.rs`, `CLAUDE.md`, `README.md`.

  Verify: `language_for_path_maps_extensions` covers the new extensions;
  `cargo fmt --all -- --check`, `RUSTFLAGS="-D warnings" cargo clippy
  --all-targets --all-features`, `cargo test --all-features` all green; an
  end-to-end `record --file app.py:<decorator line>` then `why app.py:<line>`
  resolves `exact` to the decorated function.
