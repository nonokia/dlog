//! Record-time anchor observation extraction (design §10.2, §10.4).
//!
//! This is the language-dependent layer (§10.5): it parses source with
//! tree-sitter and reports observations about **named definition nodes**
//! (functions, methods, structs/classes, enums, traits/interfaces, modules). It
//! does *not* judge identity — that happens at query time and is the resolver's
//! job (#8). It only reports what is observed now: `symbol_path`, `node_kind`,
//! `line_span`, and a `structural_hash`.
//!
//! The per-language knowledge lives in [`LangSupport`]; the walk and hashing are
//! language-agnostic. Rust, TypeScript/TSX, Go, PHP, Python, Java, and Ruby are
//! supported (§10.6); other files have no [`language_for_path`] and degrade to
//! file-level anchors (§10.5).
//!
//! The `structural_hash` is a stable FNV-1a hash over the node's token stream
//! with **identifiers, comments, and whitespace normalised away**, seeded with
//! name-free structural discriminators (kind + arity + return-type presence). So
//! renaming a function or its locals leaves the hash unchanged (enabling
//! `relocated`, §10.3) while structure/literal changes break it (`drifted`).

use tree_sitter::{Node, Parser};

/// A named definition node observed in a source file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Definition {
    /// Name-based coordinate, e.g. `AuthService::authenticate` (§10.2). The `::`
    /// join is just an internal coordinate, not language syntax.
    pub symbol_path: String,
    /// `function` | `method` | `struct` | `class` | `enum` | `union` | `trait` |
    /// `interface` | `module`.
    pub node_kind: String,
    /// 1-based inclusive row span (human snapshot, §10.2).
    pub line_span: (u32, u32),
    pub structural_hash: String,
}

/// Per-language knowledge needed to extract definitions. Everything else (the
/// tree walk, symbol-path building, hashing) is shared.
pub struct LangSupport {
    language: fn() -> tree_sitter::Language,
    /// Map a definition node to the label we anchor to, or `None`. Takes the
    /// whole node (not just its kind) so languages can disambiguate by children
    /// — e.g. Go's `type_spec` is a `struct` or `interface` depending on the
    /// `type` child.
    classify: fn(Node, bool) -> Option<&'static str>,
    /// The path segment a node contributes to enclosed definitions (e.g. a
    /// module/class name, or an `impl` block's type).
    scope_segment: fn(Node, &[u8]) -> Option<String>,
    /// An extra path segment for the definition node *itself* (not inherited by
    /// children), used when a definition carries its own owner — e.g. a Go
    /// method's receiver type yields `Client::Connect`. Most languages return
    /// `None` and rely on [`LangSupport::scope_segment`] alone.
    def_prefix: fn(Node, &[u8]) -> Option<String>,
    /// The node whose rows become the reported [`Definition::line_span`], given
    /// the definition node. Almost always the definition itself; Python returns
    /// the enclosing `decorated_definition` so the decorator lines count as part
    /// of the definition. The *hash* is always taken over the definition node,
    /// so this only widens the human snapshot (§10.2).
    span_node: fn(Node) -> Node,
    /// Whether descending into this node makes its functions *methods*.
    opens_method_scope: fn(&str) -> bool,
    /// Whether descending into this node ends method context (e.g. a function).
    resets_method_scope: fn(&str) -> bool,
    is_identifier: fn(&str) -> bool,
    is_comment: fn(&str) -> bool,
}

/// Pick the language support for a path by file extension, or `None` when the
/// file has no node-anchoring support (it degrades to file level, §10.5).
pub fn language_for_path(path: &str) -> Option<&'static LangSupport> {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    let ext = name.rsplit_once('.').map(|(_, e)| e)?;
    match ext {
        "rs" => Some(&RUST),
        "ts" | "mts" | "cts" => Some(&TYPESCRIPT),
        "tsx" => Some(&TSX),
        "go" => Some(&GO),
        "php" | "phtml" => Some(&PHP),
        "py" | "pyi" => Some(&PYTHON),
        "java" => Some(&JAVA),
        "rb" => Some(&RUBY),
        _ => None,
    }
}

/// Extract every named definition node from `source` for `lang` (§10.4). Returns
/// an empty vec if the language can't be loaded or the source can't be parsed —
/// extraction never errors; callers degrade to file-level anchors (§10.5).
pub fn extract_definitions(source: &str, lang: &LangSupport) -> Vec<Definition> {
    let mut parser = Parser::new();
    if parser.set_language(&(lang.language)()).is_err() {
        return Vec::new();
    }
    let Some(tree) = parser.parse(source, None) else {
        return Vec::new();
    };
    let bytes = source.as_bytes();
    let mut out = Vec::new();
    let mut scope: Vec<String> = Vec::new();
    walk(tree.root_node(), bytes, lang, &mut scope, false, &mut out);
    out
}

/// The innermost definition whose span encloses the given 1-based `line`, if any.
/// Used to resolve `dlog why file:line` to "the smallest definition around that
/// line" (§9.2, §10.4).
pub fn definition_at_line(source: &str, line: u32, lang: &LangSupport) -> Option<Definition> {
    extract_definitions(source, lang)
        .into_iter()
        .filter(|d| d.line_span.0 <= line && line <= d.line_span.1)
        .min_by_key(|d| d.line_span.1 - d.line_span.0)
}

fn walk(
    node: Node,
    src: &[u8],
    lang: &LangSupport,
    scope: &mut Vec<String>,
    in_method: bool,
    out: &mut Vec<Definition>,
) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        let kind = child.kind();

        if let Some(def_kind) = (lang.classify)(child, in_method)
            && let Some(name) = node_name(child, src)
        {
            let mut path = scope.clone();
            if let Some(prefix) = (lang.def_prefix)(child, src) {
                path.push(prefix);
            }
            path.push(name);
            let span = (lang.span_node)(child);
            out.push(Definition {
                symbol_path: path.join("::"),
                node_kind: def_kind.to_string(),
                line_span: (
                    span.start_position().row as u32 + 1,
                    span.end_position().row as u32 + 1,
                ),
                structural_hash: structural_hash(child, src, lang),
            });
        }

        // Recurse, updating the symbol-path scope and method context.
        let pushed = (lang.scope_segment)(child, src).inspect(|seg| scope.push(seg.clone()));
        let child_in_method = if (lang.opens_method_scope)(kind) {
            true
        } else if (lang.resets_method_scope)(kind) {
            false
        } else {
            in_method
        };
        walk(child, src, lang, scope, child_in_method, out);
        if pushed.is_some() {
            scope.pop();
        }
    }
}

fn node_name(node: Node, src: &[u8]) -> Option<String> {
    node.child_by_field_name("name")
        .and_then(|n| n.utf8_text(src).ok())
        .map(str::to_string)
}

// ---- Rust -----------------------------------------------------------------

pub static RUST: LangSupport = LangSupport {
    language: rust_language,
    classify: rust_classify,
    scope_segment: rust_scope_segment,
    def_prefix: |_, _| None,
    span_node: |n| n,
    opens_method_scope: |k| matches!(k, "impl_item" | "trait_item"),
    resets_method_scope: |k| matches!(k, "mod_item" | "function_item"),
    is_identifier: |k| {
        matches!(
            k,
            "identifier" | "type_identifier" | "field_identifier" | "shorthand_field_identifier"
        )
    },
    is_comment: |k| matches!(k, "line_comment" | "block_comment"),
};

fn rust_language() -> tree_sitter::Language {
    tree_sitter_rust::LANGUAGE.into()
}

fn rust_classify(node: Node, in_method: bool) -> Option<&'static str> {
    match node.kind() {
        // `function_signature_item` is a body-less method declaration in a trait.
        "function_item" | "function_signature_item" => {
            Some(if in_method { "method" } else { "function" })
        }
        "struct_item" => Some("struct"),
        "enum_item" => Some("enum"),
        "union_item" => Some("union"),
        "trait_item" => Some("trait"),
        "mod_item" => Some("module"),
        _ => None,
    }
}

fn rust_scope_segment(node: Node, src: &[u8]) -> Option<String> {
    match node.kind() {
        "mod_item" | "trait_item" => node_name(node, src),
        "impl_item" => node
            .child_by_field_name("type")
            .and_then(|t| t.utf8_text(src).ok())
            .map(str::to_string),
        _ => None,
    }
}

// ---- TypeScript / TSX ------------------------------------------------------

pub static TYPESCRIPT: LangSupport = LangSupport {
    language: ts_language,
    classify: ts_classify,
    scope_segment: ts_scope_segment,
    def_prefix: |_, _| None,
    span_node: |n| n,
    opens_method_scope: ts_opens_method_scope,
    resets_method_scope: ts_resets_method_scope,
    is_identifier: ts_is_identifier,
    is_comment: |k| k == "comment",
};

pub static TSX: LangSupport = LangSupport {
    language: tsx_language,
    classify: ts_classify,
    scope_segment: ts_scope_segment,
    def_prefix: |_, _| None,
    span_node: |n| n,
    opens_method_scope: ts_opens_method_scope,
    resets_method_scope: ts_resets_method_scope,
    is_identifier: ts_is_identifier,
    is_comment: |k| k == "comment",
};

fn ts_language() -> tree_sitter::Language {
    tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()
}

fn tsx_language() -> tree_sitter::Language {
    tree_sitter_typescript::LANGUAGE_TSX.into()
}

fn ts_classify(node: Node, in_method: bool) -> Option<&'static str> {
    match node.kind() {
        "function_declaration" | "generator_function_declaration" => {
            Some(if in_method { "method" } else { "function" })
        }
        "method_definition" | "method_signature" | "abstract_method_signature" => Some("method"),
        "class_declaration" | "abstract_class_declaration" => Some("class"),
        "interface_declaration" => Some("interface"),
        "enum_declaration" => Some("enum"),
        "internal_module" => Some("module"), // `namespace X {}` / `module X {}`
        _ => None,
    }
}

fn ts_scope_segment(node: Node, src: &[u8]) -> Option<String> {
    match node.kind() {
        "class_declaration"
        | "abstract_class_declaration"
        | "interface_declaration"
        | "internal_module" => node_name(node, src),
        _ => None,
    }
}

fn ts_opens_method_scope(kind: &str) -> bool {
    matches!(
        kind,
        "class_declaration" | "abstract_class_declaration" | "interface_declaration"
    )
}

fn ts_resets_method_scope(kind: &str) -> bool {
    matches!(
        kind,
        "function_declaration"
            | "generator_function_declaration"
            | "method_definition"
            | "method_signature"
            | "abstract_method_signature"
            | "internal_module"
    )
}

fn ts_is_identifier(kind: &str) -> bool {
    matches!(
        kind,
        "identifier"
            | "type_identifier"
            | "property_identifier"
            | "shorthand_property_identifier"
            | "private_property_identifier"
    )
}

// ---- Go --------------------------------------------------------------------

pub static GO: LangSupport = LangSupport {
    language: go_language,
    classify: go_classify,
    scope_segment: go_scope_segment,
    def_prefix: go_def_prefix,
    // Go methods are top-level `method_declaration`s carrying a receiver, not
    // nested in the type, so method context is decided per-node in `go_classify`
    // rather than by descending into a scope.
    span_node: |n| n,
    opens_method_scope: |_| false,
    resets_method_scope: |_| false,
    is_identifier: |k| {
        matches!(
            k,
            "identifier" | "type_identifier" | "field_identifier" | "package_identifier"
        )
    },
    is_comment: |k| k == "comment",
};

fn go_language() -> tree_sitter::Language {
    tree_sitter_go::LANGUAGE.into()
}

fn go_classify(node: Node, _in_method: bool) -> Option<&'static str> {
    match node.kind() {
        "function_declaration" => Some("function"),
        // A free `method_declaration` (receiver-based) or an `interface`'s
        // `method_elem` signature both anchor as methods.
        "method_declaration" | "method_elem" => Some("method"),
        // `type Foo struct/interface {…}`: the shape lives in the `type` child.
        "type_spec" => match node.child_by_field_name("type").map(|t| t.kind()) {
            Some("struct_type") => Some("struct"),
            Some("interface_type") => Some("interface"),
            _ => None,
        },
        _ => None,
    }
}

fn go_scope_segment(node: Node, src: &[u8]) -> Option<String> {
    // An interface's method signatures nest under the type name (`Transport::Send`).
    // Struct methods don't nest here — they're top-level and use `go_def_prefix`.
    match node.kind() {
        "type_spec" => node_name(node, src),
        _ => None,
    }
}

/// A Go method's receiver type, e.g. `Connect` on `func (c *Client) Connect()`
/// becomes `Client::Connect`. Strips a leading `*` on pointer receivers.
fn go_def_prefix(node: Node, src: &[u8]) -> Option<String> {
    if node.kind() != "method_declaration" {
        return None;
    }
    let receiver = node.child_by_field_name("receiver")?;
    let mut cursor = receiver.walk();
    let param = receiver
        .children(&mut cursor)
        .find(|c| c.kind() == "parameter_declaration")?;
    let ty = param.child_by_field_name("type")?;
    // Unwrap `*T` to `T`.
    let base = if ty.kind() == "pointer_type" {
        ty.named_child(0).unwrap_or(ty)
    } else {
        ty
    };
    base.utf8_text(src).ok().map(str::to_string)
}

// ---- PHP -------------------------------------------------------------------

pub static PHP: LangSupport = LangSupport {
    language: php_language,
    classify: php_classify,
    scope_segment: php_scope_segment,
    def_prefix: |_, _| None,
    span_node: |n| n,
    opens_method_scope: |k| {
        matches!(
            k,
            "class_declaration"
                | "interface_declaration"
                | "trait_declaration"
                | "enum_declaration"
        )
    },
    resets_method_scope: |k| matches!(k, "function_definition" | "method_declaration"),
    // In PHP every identifier — class/function names, type references, the ident
    // inside a `variable_name` — is a `name` leaf.
    is_identifier: |k| k == "name",
    is_comment: |k| k == "comment",
};

fn php_language() -> tree_sitter::Language {
    tree_sitter_php::LANGUAGE_PHP.into()
}

fn php_classify(node: Node, _in_method: bool) -> Option<&'static str> {
    match node.kind() {
        // A class body uses `method_declaration`; free functions are
        // `function_definition`.
        "function_definition" => Some("function"),
        "method_declaration" => Some("method"),
        "class_declaration" => Some("class"),
        "interface_declaration" => Some("interface"),
        "trait_declaration" => Some("trait"),
        "enum_declaration" => Some("enum"),
        "namespace_definition" => Some("module"),
        _ => None,
    }
}

fn php_scope_segment(node: Node, src: &[u8]) -> Option<String> {
    match node.kind() {
        "class_declaration"
        | "interface_declaration"
        | "trait_declaration"
        | "enum_declaration"
        // Block-form `namespace X { … }` nests its members; the statement form
        // `namespace X;` puts them as siblings and simply doesn't prefix (§10.5).
        | "namespace_definition" => node_name(node, src),
        _ => None,
    }
}

// ---- Python ----------------------------------------------------------------

pub static PYTHON: LangSupport = LangSupport {
    language: python_language,
    classify: python_classify,
    scope_segment: python_scope_segment,
    def_prefix: |_, _| None,
    span_node: python_span_node,
    opens_method_scope: |k| k == "class_definition",
    resets_method_scope: |k| k == "function_definition",
    is_identifier: |k| k == "identifier",
    is_comment: |k| k == "comment",
};

fn python_language() -> tree_sitter::Language {
    tree_sitter_python::LANGUAGE.into()
}

fn python_classify(node: Node, in_method: bool) -> Option<&'static str> {
    match node.kind() {
        "function_definition" => Some(if in_method { "method" } else { "function" }),
        "class_definition" => Some("class"),
        // A `.py` file is itself the module; there is no module node to anchor.
        _ => None,
    }
}

fn python_scope_segment(node: Node, src: &[u8]) -> Option<String> {
    match node.kind() {
        "class_definition" => node_name(node, src),
        // Inner functions nest too — unlike the other languages, they are
        // idiomatic here (every decorator has a `wrapper`), so leaving them
        // unqualified would collide several of them in one file.
        "function_definition" => node_name(node, src),
        _ => None,
    }
}

/// Widen a decorated definition's span to include its decorators. In Python the
/// grammar puts `@decorator` lines in a `decorated_definition` *wrapping* the
/// `function_definition`, so the definition node alone starts below them — and
/// `dlog why file:line` on a decorator line would then find nothing (§10.4).
fn python_span_node(node: Node) -> Node {
    match node.parent() {
        Some(parent) if parent.kind() == "decorated_definition" => parent,
        _ => node,
    }
}

// ---- Java ------------------------------------------------------------------

pub static JAVA: LangSupport = LangSupport {
    language: java_language,
    classify: java_classify,
    scope_segment: java_scope_segment,
    def_prefix: |_, _| None,
    span_node: |n| n,
    // Java has no free functions: a `method_declaration` is always a method, so
    // there is no method scope to open or reset.
    opens_method_scope: |_| false,
    resets_method_scope: |_| false,
    is_identifier: |k| matches!(k, "identifier" | "type_identifier"),
    is_comment: |k| matches!(k, "line_comment" | "block_comment" | "comment"),
};

fn java_language() -> tree_sitter::Language {
    tree_sitter_java::LANGUAGE.into()
}

fn java_classify(node: Node, _in_method: bool) -> Option<&'static str> {
    match node.kind() {
        // A constructor is a method whose name is the type's; anchoring it lets a
        // decision about construction land on the constructor, not the class.
        "method_declaration" | "constructor_declaration" | "compact_constructor_declaration" => {
            Some("method")
        }
        "class_declaration" | "record_declaration" => Some("class"),
        // `@interface Foo` is an annotation type; it anchors like an interface.
        "interface_declaration" | "annotation_type_declaration" => Some("interface"),
        "enum_declaration" => Some("enum"),
        _ => None,
    }
}

fn java_scope_segment(node: Node, src: &[u8]) -> Option<String> {
    match node.kind() {
        "class_declaration"
        | "record_declaration"
        | "interface_declaration"
        | "annotation_type_declaration"
        | "enum_declaration" => node_name(node, src),
        _ => None,
    }
}

// ---- Ruby ------------------------------------------------------------------

pub static RUBY: LangSupport = LangSupport {
    language: ruby_language,
    classify: ruby_classify,
    scope_segment: ruby_scope_segment,
    def_prefix: ruby_def_prefix,
    span_node: |n| n,
    opens_method_scope: |k| matches!(k, "class" | "module" | "singleton_class"),
    resets_method_scope: |k| matches!(k, "method" | "singleton_method"),
    is_identifier: |k| {
        matches!(
            k,
            "identifier"
                | "constant"
                | "instance_variable"
                | "class_variable"
                | "global_variable"
                | "hash_key_symbol"
        )
    },
    is_comment: |k| k == "comment",
};

fn ruby_language() -> tree_sitter::Language {
    tree_sitter_ruby::LANGUAGE.into()
}

fn ruby_classify(node: Node, in_method: bool) -> Option<&'static str> {
    match node.kind() {
        // A top-level `def` is a private method on Object; calling it a function
        // matches how the other languages label an unowned definition.
        "method" => Some(if in_method { "method" } else { "function" }),
        // `def self.create` / `def Foo.create` always belong to an owner.
        "singleton_method" => Some("method"),
        "class" => Some("class"),
        "module" => Some("module"),
        _ => None,
    }
}

fn ruby_scope_segment(node: Node, src: &[u8]) -> Option<String> {
    match node.kind() {
        // `class Foo::Bar` yields a `scope_resolution` name whose text is already
        // `Foo::Bar` — the same separator the symbol path uses.
        "class" | "module" => node_name(node, src),
        // `class << self` — the receiver becomes a path segment, so its methods
        // read `Client::self::create`, matching `def self.create` below.
        "singleton_class" => node
            .child_by_field_name("value")
            .and_then(|v| v.utf8_text(src).ok())
            .map(str::to_string),
        _ => None,
    }
}

/// A singleton method's receiver, e.g. `create` in `def self.create` under
/// `class Client` becomes `Client::self::create`. Keeping `self` in the path is
/// what separates a class method from a same-named instance method.
fn ruby_def_prefix(node: Node, src: &[u8]) -> Option<String> {
    if node.kind() != "singleton_method" {
        return None;
    }
    node.child_by_field_name("object")
        .and_then(|o| o.utf8_text(src).ok())
        .map(str::to_string)
}

// ---- Structural hash (language-agnostic) ----------------------------------

/// Stable FNV-1a hash of a node's normalised token stream, seeded with name-free
/// structural discriminators (kind + parameter arity + return-type presence).
/// The seed never reads identifier/type *names*, so renames stay invariant while
/// trivially same-shaped nodes don't collide on the token stream alone
/// (§10.3, issue #28).
fn structural_hash(node: Node, src: &[u8], lang: &LangSupport) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    fnv_write(&mut hash, node.kind().as_bytes());
    fnv_write(&mut hash, b"\x1e");
    fnv_write(&mut hash, &signature_shape(node).to_le_bytes());
    fnv_write(&mut hash, b"\x1e");
    hash_tokens(node, src, lang, &mut hash);
    format!("{hash:016x}")
}

/// A name-free shape signature: parameter count and whether a return type is
/// present, packed into one integer. The parameter list is found by field name
/// (`parameters`, shared by Rust, TS, Go, and PHP); its named children are the
/// params. The return type is `return_type` in every grammar except Go, which
/// names it `result`.
fn signature_shape(node: Node) -> u32 {
    let arity = node
        .child_by_field_name("parameters")
        .map(|params| params.named_child_count() as u32)
        .unwrap_or(0);
    let has_return = node.child_by_field_name("return_type").is_some()
        || node.child_by_field_name("result").is_some();
    (arity << 1) | has_return as u32
}

fn hash_tokens(node: Node, src: &[u8], lang: &LangSupport, hash: &mut u64) {
    let kind = node.kind();
    // Comments are ignored entirely. They aren't always leaves (e.g. Rust's
    // `line_comment` wraps a `//`), so this must come before the leaf check.
    if (lang.is_comment)(kind) {
        return;
    }
    if node.child_count() == 0 {
        if (lang.is_identifier)(kind) {
            // Collapse every identifier to one placeholder so names don't matter.
            fnv_write(hash, b"\x01id");
        } else if let Ok(text) = node.utf8_text(src) {
            // Keywords, punctuation, operators, and literals are kept verbatim.
            fnv_write(hash, text.as_bytes());
        }
        fnv_write(hash, b"\x1f"); // token separator
        return;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        hash_tokens(child, src, lang, hash);
    }
}

fn fnv_write(hash: &mut u64, bytes: &[u8]) {
    for b in bytes {
        *hash ^= *b as u64;
        *hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hash_of_single_def(source: &str) -> String {
        let defs = extract_definitions(source, &RUST);
        assert_eq!(defs.len(), 1, "expected exactly one definition");
        defs.into_iter().next().unwrap().structural_hash
    }

    #[test]
    fn language_for_path_maps_extensions() {
        assert!(language_for_path("src/a.rs").is_some());
        assert!(language_for_path("src/a.ts").is_some());
        assert!(language_for_path("src/a.tsx").is_some());
        assert!(language_for_path("src/a.go").is_some());
        assert!(language_for_path("src/a.php").is_some());
        assert!(language_for_path("src/a.py").is_some());
        assert!(language_for_path("src/a.pyi").is_some());
        assert!(language_for_path("src/A.java").is_some());
        assert!(language_for_path("src/a.rb").is_some());
        assert!(language_for_path("README.md").is_none());
        assert!(language_for_path("Makefile").is_none());
        // A dot in a directory must not be mistaken for an extension.
        assert!(language_for_path("a.b/Makefile").is_none());
    }

    #[test]
    fn extracts_nested_symbol_paths_and_kinds() {
        let source = r#"
mod net {
    pub struct Client { url: String }
    impl Client {
        fn connect(&self) -> bool { true }
    }
    pub trait Transport {
        fn send(&self);
    }
}

fn main() {}
"#;
        let defs = extract_definitions(source, &RUST);
        let by_path: Vec<(&str, &str)> = defs
            .iter()
            .map(|d| (d.symbol_path.as_str(), d.node_kind.as_str()))
            .collect();

        assert!(by_path.contains(&("net", "module")));
        assert!(by_path.contains(&("net::Client", "struct")));
        assert!(by_path.contains(&("net::Client::connect", "method")));
        assert!(by_path.contains(&("net::Transport", "trait")));
        assert!(by_path.contains(&("net::Transport::send", "method")));
        assert!(by_path.contains(&("main", "function")));
    }

    #[test]
    fn extracts_typescript_definitions() {
        let source = r#"
namespace net {
  export class Client {
    connect(n: number): boolean { return n > 0; }
  }
  export interface Transport { send(x: string): void; }
  export function helper(a: number, b: number) { return a + b; }
}
function top() {}
"#;
        let defs = extract_definitions(source, &TYPESCRIPT);
        let by_path: Vec<(&str, &str)> = defs
            .iter()
            .map(|d| (d.symbol_path.as_str(), d.node_kind.as_str()))
            .collect();

        assert!(by_path.contains(&("net", "module")));
        assert!(by_path.contains(&("net::Client", "class")));
        assert!(by_path.contains(&("net::Client::connect", "method")));
        assert!(by_path.contains(&("net::Transport", "interface")));
        assert!(by_path.contains(&("net::Transport::send", "method")));
        assert!(by_path.contains(&("net::helper", "function")));
        assert!(by_path.contains(&("top", "function")));
    }

    #[test]
    fn typescript_definition_at_line_and_hash_invariance() {
        let source = "class C {\n  m(a: number): void {\n    let x = 1;\n  }\n}\n";
        let def = definition_at_line(source, 3, &TYPESCRIPT).expect("line 3 inside C::m");
        assert_eq!(def.symbol_path, "C::m");
        assert_eq!(def.node_kind, "method");

        // Identifier renames don't change the TS hash; an arity change does.
        let h = |s: &str| {
            extract_definitions(s, &TYPESCRIPT)
                .into_iter()
                .next()
                .unwrap()
                .structural_hash
        };
        assert_eq!(
            h("function f(a: number) { return a; }"),
            h("function renamed(b: number) { return b; }")
        );
        assert_ne!(
            h("function f(a: number) {}"),
            h("function f(a: number, b: number) {}")
        );
    }

    #[test]
    fn extracts_go_definitions() {
        let source = r#"
package net

type Client struct { url string }

func (c *Client) Connect(n int) bool { return n > 0 }

func Helper(a int, b int) int { return a + b }

type Transport interface {
    Send(x string)
}
"#;
        let defs = extract_definitions(source, &GO);
        let by_path: Vec<(&str, &str)> = defs
            .iter()
            .map(|d| (d.symbol_path.as_str(), d.node_kind.as_str()))
            .collect();

        assert!(by_path.contains(&("Client", "struct")));
        // Receiver type is prepended even though the method is top-level.
        assert!(by_path.contains(&("Client::Connect", "method")));
        assert!(by_path.contains(&("Helper", "function")));
        assert!(by_path.contains(&("Transport", "interface")));
        assert!(by_path.contains(&("Transport::Send", "method")));
    }

    #[test]
    fn go_definition_at_line_and_hash_invariance() {
        let source = "package p\nfunc (c *Client) m(a int) int {\n  x := a\n  return x\n}\n";
        let def = definition_at_line(source, 3, &GO).expect("line 3 inside Client::m");
        assert_eq!(def.symbol_path, "Client::m");
        assert_eq!(def.node_kind, "method");

        // Identifier renames don't change the Go hash; an arity change does.
        let h = |s: &str| {
            extract_definitions(s, &GO)
                .into_iter()
                .next()
                .unwrap()
                .structural_hash
        };
        assert_eq!(
            h("package p\nfunc f(a int) int { return a }"),
            h("package p\nfunc renamed(b int) int { return b }")
        );
        assert_ne!(
            h("package p\nfunc f(a int) {}"),
            h("package p\nfunc f(a int, b int) {}")
        );
    }

    #[test]
    fn extracts_php_definitions() {
        let source = r#"<?php
namespace App {
    class Client {
        public function connect(int $n): bool { return $n > 0; }
    }
    interface Transport { public function send(string $x): void; }
    trait Loggable { public function log(): void {} }
    function helper(int $a, int $b): int { return $a + $b; }
}
"#;
        let defs = extract_definitions(source, &PHP);
        let by_path: Vec<(&str, &str)> = defs
            .iter()
            .map(|d| (d.symbol_path.as_str(), d.node_kind.as_str()))
            .collect();

        assert!(by_path.contains(&("App", "module")));
        assert!(by_path.contains(&("App::Client", "class")));
        assert!(by_path.contains(&("App::Client::connect", "method")));
        assert!(by_path.contains(&("App::Transport", "interface")));
        assert!(by_path.contains(&("App::Transport::send", "method")));
        assert!(by_path.contains(&("App::Loggable", "trait")));
        assert!(by_path.contains(&("App::Loggable::log", "method")));
        assert!(by_path.contains(&("App::helper", "function")));
    }

    #[test]
    fn php_definition_at_line_and_hash_invariance() {
        let source = "<?php\nclass C {\n  function m(int $a): void {\n    $x = 1;\n  }\n}\n";
        let def = definition_at_line(source, 4, &PHP).expect("line 4 inside C::m");
        assert_eq!(def.symbol_path, "C::m");
        assert_eq!(def.node_kind, "method");

        // Identifier renames don't change the PHP hash; an arity change does.
        let h = |s: &str| {
            extract_definitions(s, &PHP)
                .into_iter()
                .next()
                .unwrap()
                .structural_hash
        };
        assert_eq!(
            h("<?php function f(int $a) { return $a; }"),
            h("<?php function renamed(int $b) { return $b; }")
        );
        assert_ne!(
            h("<?php function f(int $a) {}"),
            h("<?php function f(int $a, int $b) {}")
        );
    }

    #[test]
    fn extracts_python_definitions() {
        let source = r#"
class Client:
    def connect(self, n):
        return n > 0

    @staticmethod
    def build(url):
        return Client()

class Nested:
    class Inner:
        def deep(self):
            pass

def helper(a, b):
    def local():
        pass
    return a + b
"#;
        let defs = extract_definitions(source, &PYTHON);
        let by_path: Vec<(&str, &str)> = defs
            .iter()
            .map(|d| (d.symbol_path.as_str(), d.node_kind.as_str()))
            .collect();

        assert!(by_path.contains(&("Client", "class")));
        assert!(by_path.contains(&("Client::connect", "method")));
        assert!(by_path.contains(&("Client::build", "method")));
        assert!(by_path.contains(&("Nested::Inner", "class")));
        assert!(by_path.contains(&("Nested::Inner::deep", "method")));
        assert!(by_path.contains(&("helper", "function")));
        // A nested `def` inside a function is a function, not a method.
        assert!(by_path.contains(&("helper::local", "function")));
    }

    #[test]
    fn python_decorator_lines_belong_to_the_definition() {
        // The `@decorator` line is above `def`, and pointing at it must still
        // resolve to the decorated function (span widened past the def node).
        let source = "@app.route(\"/\")\n@cached\ndef index():\n    return 1\n";
        for line in 1..=4 {
            let def = definition_at_line(source, line, &PYTHON)
                .unwrap_or_else(|| panic!("line {line} should be inside index"));
            assert_eq!(def.symbol_path, "index");
        }
        let def = definition_at_line(source, 1, &PYTHON).unwrap();
        assert_eq!(def.line_span, (1, 4));
    }

    #[test]
    fn python_definition_at_line_and_hash_invariance() {
        let source = "class C:\n    def m(self, a):\n        x = 1\n        return x\n";
        let def = definition_at_line(source, 3, &PYTHON).expect("line 3 inside C::m");
        assert_eq!(def.symbol_path, "C::m");
        assert_eq!(def.node_kind, "method");

        // Identifier renames don't change the Python hash; an arity change does.
        let h = |s: &str| {
            extract_definitions(s, &PYTHON)
                .into_iter()
                .next()
                .unwrap()
                .structural_hash
        };
        assert_eq!(
            h("def f(a):\n    return a\n"),
            h("def renamed(b):\n    return b\n")
        );
        assert_ne!(h("def f(a):\n    pass\n"), h("def f(a, b):\n    pass\n"));
        // Comments and reflowed whitespace stay invariant.
        assert_eq!(
            h("def f(a):\n    return a + 1\n"),
            h("def f(a):\n    # add one\n    return a  +  1\n")
        );
    }

    #[test]
    fn extracts_java_definitions() {
        let source = r#"
package net;

public class Client implements Transport {
    Client(String url) { this.url = url; }
    public boolean connect(int n) { return n > 0; }
}

interface Transport { void send(String x); }

enum Mode { FAST, SLOW }

record Point(int x, int y) {}
"#;
        let defs = extract_definitions(source, &JAVA);
        let by_path: Vec<(&str, &str)> = defs
            .iter()
            .map(|d| (d.symbol_path.as_str(), d.node_kind.as_str()))
            .collect();

        assert!(by_path.contains(&("Client", "class")));
        assert!(
            by_path.contains(&("Client::Client", "method")),
            "constructor"
        );
        assert!(by_path.contains(&("Client::connect", "method")));
        assert!(by_path.contains(&("Transport", "interface")));
        assert!(by_path.contains(&("Transport::send", "method")));
        assert!(by_path.contains(&("Mode", "enum")));
        assert!(by_path.contains(&("Point", "class")));
    }

    #[test]
    fn java_definition_at_line_and_hash_invariance() {
        let source = "class C {\n  void m(int a) {\n    int x = a;\n  }\n}\n";
        let def = definition_at_line(source, 3, &JAVA).expect("line 3 inside C::m");
        assert_eq!(def.symbol_path, "C::m");
        assert_eq!(def.node_kind, "method");

        // Identifier renames don't change the Java hash; an arity change does.
        let h = |s: &str| {
            extract_definitions(s, &JAVA)
                .into_iter()
                .find(|d| d.node_kind == "method")
                .unwrap()
                .structural_hash
        };
        assert_eq!(
            h("class C { int f(int a) { return a; } }"),
            h("class D { int renamed(int b) { return b; } }")
        );
        assert_ne!(
            h("class C { void f(int a) {} }"),
            h("class C { void f(int a, int b) {} }")
        );
    }

    #[test]
    fn extracts_ruby_definitions() {
        let source = r#"
module Net
  class Client
    def connect(n)
      n > 0
    end

    def self.build(url)
      new(url)
    end

    class << self
      def registry
        @registry
      end
    end
  end
end

def helper(a, b)
  a + b
end
"#;
        let defs = extract_definitions(source, &RUBY);
        let by_path: Vec<(&str, &str)> = defs
            .iter()
            .map(|d| (d.symbol_path.as_str(), d.node_kind.as_str()))
            .collect();

        assert!(by_path.contains(&("Net", "module")));
        assert!(by_path.contains(&("Net::Client", "class")));
        assert!(by_path.contains(&("Net::Client::connect", "method")));
        // `self` stays in the path so a class method can't collide with a
        // same-named instance method.
        assert!(by_path.contains(&("Net::Client::self::build", "method")));
        assert!(by_path.contains(&("Net::Client::self::registry", "method")));
        // A top-level `def` has no owner.
        assert!(by_path.contains(&("helper", "function")));
    }

    #[test]
    fn ruby_definition_at_line_and_hash_invariance() {
        let source = "class C\n  def m(a)\n    x = a\n    x\n  end\nend\n";
        let def = definition_at_line(source, 3, &RUBY).expect("line 3 inside C::m");
        assert_eq!(def.symbol_path, "C::m");
        assert_eq!(def.node_kind, "method");

        // Identifier renames don't change the Ruby hash; an arity change does.
        let h = |s: &str| {
            extract_definitions(s, &RUBY)
                .into_iter()
                .find(|d| d.node_kind != "class")
                .unwrap()
                .structural_hash
        };
        assert_eq!(h("def f(a)\n  a\nend\n"), h("def renamed(b)\n  b\nend\n"));
        assert_ne!(h("def f(a)\nend\n"), h("def f(a, b)\nend\n"));
        // Instance-variable renames are identifier renames too.
        assert_eq!(
            h("def f(a)\n  @url = a\nend\n"),
            h("def f(a)\n  @host = a\nend\n")
        );
    }

    #[test]
    fn structural_hash_ignores_identifier_renames() {
        let a = "fn f(x: u32) -> u32 { let y = x + 1; y }";
        let b = "fn renamed(arg: u32) -> u32 { let out = arg + 1; out }";
        assert_eq!(hash_of_single_def(a), hash_of_single_def(b));
    }

    #[test]
    fn structural_hash_distinguishes_arity_and_return() {
        // Same token-shaped bodies but different signatures must not collide
        // (the seed adds arity / return-type presence; #28).
        let no_args = "fn f() { let x = 1; }";
        let one_arg = "fn f(a: u32) { let x = 1; }";
        let two_args = "fn f(a: u32, b: u32) { let x = 1; }";
        assert_ne!(hash_of_single_def(no_args), hash_of_single_def(one_arg));
        assert_ne!(hash_of_single_def(one_arg), hash_of_single_def(two_args));

        let no_ret = "fn f(a: u32) { let x = 1; }";
        let with_ret = "fn f(a: u32) -> u32 { let x = 1; }";
        assert_ne!(hash_of_single_def(no_ret), hash_of_single_def(with_ret));
    }

    #[test]
    fn structural_hash_ignores_whitespace_and_comments() {
        let a = "fn f(x: u32) -> u32 { x + 1 }";
        let b = "fn f(x: u32) -> u32 {\n    // add one\n    x   +   1\n}";
        assert_eq!(hash_of_single_def(a), hash_of_single_def(b));
    }

    #[test]
    fn structural_hash_changes_with_structure_or_literals() {
        let base = "fn f(x: u32) -> u32 { x + 1 }";
        let lit = "fn f(x: u32) -> u32 { x + 2 }";
        let structure = "fn f(x: u32) -> u32 { x * 1 }";
        assert_ne!(hash_of_single_def(base), hash_of_single_def(lit));
        assert_ne!(hash_of_single_def(base), hash_of_single_def(structure));
    }

    #[test]
    fn definition_at_line_picks_innermost() {
        let source = r#"
impl Service {
    fn handle(&self) {
        let x = 1;
    }
}
"#;
        // Line 4 (`let x = 1;`) is inside the method, which is inside the impl.
        let def = definition_at_line(source, 4, &RUST).expect("a definition encloses line 4");
        assert_eq!(def.symbol_path, "Service::handle");
        assert_eq!(def.node_kind, "method");
    }

    #[test]
    fn unparsable_or_empty_yields_no_definitions() {
        assert!(extract_definitions("", &RUST).is_empty());
        assert!(definition_at_line("// just a comment", 1, &RUST).is_none());
    }
}
