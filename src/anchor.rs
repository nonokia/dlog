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
//! language-agnostic. Rust and TypeScript/TSX are supported (§10.6); other files
//! have no [`language_for_path`] and degrade to file-level anchors (§10.5).
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
    /// Map a node kind to the definition label we anchor to, or `None`.
    classify: fn(&str, bool) -> Option<&'static str>,
    /// The path segment a node contributes to enclosed definitions (e.g. a
    /// module/class name, or an `impl` block's type).
    scope_segment: fn(Node, &[u8]) -> Option<String>,
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

        if let Some(def_kind) = (lang.classify)(kind, in_method)
            && let Some(name) = node_name(child, src)
        {
            let mut path = scope.clone();
            path.push(name);
            out.push(Definition {
                symbol_path: path.join("::"),
                node_kind: def_kind.to_string(),
                line_span: (
                    child.start_position().row as u32 + 1,
                    child.end_position().row as u32 + 1,
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

fn rust_classify(kind: &str, in_method: bool) -> Option<&'static str> {
    match kind {
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
    opens_method_scope: ts_opens_method_scope,
    resets_method_scope: ts_resets_method_scope,
    is_identifier: ts_is_identifier,
    is_comment: |k| k == "comment",
};

pub static TSX: LangSupport = LangSupport {
    language: tsx_language,
    classify: ts_classify,
    scope_segment: ts_scope_segment,
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

fn ts_classify(kind: &str, in_method: bool) -> Option<&'static str> {
    match kind {
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
/// (`parameters`, shared by Rust and TS); its named children are the params.
fn signature_shape(node: Node) -> u32 {
    let arity = node
        .child_by_field_name("parameters")
        .map(|params| params.named_child_count() as u32)
        .unwrap_or(0);
    let has_return = node.child_by_field_name("return_type").is_some();
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
