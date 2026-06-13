//! Record-time anchor observation extraction for Rust (design §10.2, §10.4).
//!
//! This is the language-dependent layer (§10.5): it parses Rust with
//! tree-sitter and reports observations about **named definition nodes**
//! (functions, methods, structs, enums, traits, modules). It does *not* judge
//! identity — that happens at query time and is the resolver's job (#8). It only
//! reports what is observed now: `symbol_path`, `node_kind`, `line_span`, and a
//! `structural_hash`.
//!
//! The `structural_hash` is a stable FNV-1a hash over the node's token stream
//! with **identifiers, comments, and whitespace normalised away**. So renaming a
//! function or its locals leaves the hash unchanged (enabling `relocated`
//! detection, §10.3), while a change to the code's structure or literals changes
//! it (enabling `drifted` detection).

use tree_sitter::{Node, Parser};

/// A named definition node observed in a source file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Definition {
    /// Name-based coordinate, e.g. `AuthService::authenticate` (§10.2).
    pub symbol_path: String,
    /// `function` | `method` | `struct` | `enum` | `union` | `trait` | `module`.
    pub node_kind: String,
    /// 1-based inclusive row span (human snapshot, §10.2).
    pub line_span: (u32, u32),
    pub structural_hash: String,
}

/// Extract every named definition node from Rust `source` (§10.4). Returns an
/// empty vec if the language can't be loaded or the source can't be parsed —
/// extraction never errors, callers degrade to file-level anchors (§10.5).
pub fn extract_definitions(source: &str) -> Vec<Definition> {
    let mut parser = Parser::new();
    if parser
        .set_language(&tree_sitter_rust::LANGUAGE.into())
        .is_err()
    {
        return Vec::new();
    }
    let Some(tree) = parser.parse(source, None) else {
        return Vec::new();
    };
    let bytes = source.as_bytes();
    let mut out = Vec::new();
    let mut scope: Vec<String> = Vec::new();
    walk(tree.root_node(), bytes, &mut scope, false, &mut out);
    out
}

/// The innermost definition whose span encloses the given 1-based `line`, if any.
/// Used to resolve `dlog why file:line` to "the smallest definition around that
/// line" (§9.2, §10.4).
pub fn definition_at_line(source: &str, line: u32) -> Option<Definition> {
    extract_definitions(source)
        .into_iter()
        .filter(|d| d.line_span.0 <= line && line <= d.line_span.1)
        .min_by_key(|d| d.line_span.1 - d.line_span.0)
}

fn walk(
    node: Node,
    src: &[u8],
    scope: &mut Vec<String>,
    in_method: bool,
    out: &mut Vec<Definition>,
) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        let kind = child.kind();

        if let Some(def_kind) = definition_kind(kind, in_method)
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
                structural_hash: structural_hash(child, src),
            });
        }

        // Recurse, updating the symbol-path scope and method context.
        let pushed = scope_segment(child, src).inspect(|seg| scope.push(seg.clone()));
        let child_in_method = match kind {
            "impl_item" | "trait_item" => true,
            "mod_item" | "function_item" => false,
            _ => in_method,
        };
        walk(child, src, scope, child_in_method, out);
        if pushed.is_some() {
            scope.pop();
        }
    }
}

/// Definition node kinds we anchor to (§10.4), or `None` for everything else.
fn definition_kind(kind: &str, in_method: bool) -> Option<&'static str> {
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

/// The path segment a node contributes to enclosed definitions' symbol paths:
/// module/trait names, and the implementing type of an `impl` block.
fn scope_segment(node: Node, src: &[u8]) -> Option<String> {
    match node.kind() {
        "mod_item" | "trait_item" => node_name(node, src),
        "impl_item" => node
            .child_by_field_name("type")
            .and_then(|t| t.utf8_text(src).ok())
            .map(str::to_string),
        _ => None,
    }
}

fn node_name(node: Node, src: &[u8]) -> Option<String> {
    node.child_by_field_name("name")
        .and_then(|n| n.utf8_text(src).ok())
        .map(str::to_string)
}

/// Stable FNV-1a hash of a node's normalised token stream.
fn structural_hash(node: Node, src: &[u8]) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    hash_tokens(node, src, &mut hash);
    format!("{hash:016x}")
}

fn hash_tokens(node: Node, src: &[u8], hash: &mut u64) {
    let kind = node.kind();
    // Comments are ignored entirely. They aren't leaves (e.g. `line_comment`
    // wraps a `//` token), so this must come before the leaf check.
    if is_comment(kind) {
        return;
    }
    if node.child_count() == 0 {
        if is_identifier(kind) {
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
        hash_tokens(child, src, hash);
    }
}

fn fnv_write(hash: &mut u64, bytes: &[u8]) {
    for b in bytes {
        *hash ^= *b as u64;
        *hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
}

fn is_identifier(kind: &str) -> bool {
    matches!(
        kind,
        "identifier" | "type_identifier" | "field_identifier" | "shorthand_field_identifier"
    )
}

fn is_comment(kind: &str) -> bool {
    matches!(kind, "line_comment" | "block_comment")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hash_of_single_def(source: &str) -> String {
        let defs = extract_definitions(source);
        assert_eq!(defs.len(), 1, "expected exactly one definition");
        defs.into_iter().next().unwrap().structural_hash
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
        let defs = extract_definitions(source);
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
    fn structural_hash_ignores_identifier_renames() {
        let a = "fn f(x: u32) -> u32 { let y = x + 1; y }";
        let b = "fn renamed(arg: u32) -> u32 { let out = arg + 1; out }";
        assert_eq!(hash_of_single_def(a), hash_of_single_def(b));
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
        let def = definition_at_line(source, 4).expect("a definition encloses line 4");
        assert_eq!(def.symbol_path, "Service::handle");
        assert_eq!(def.node_kind, "method");
    }

    #[test]
    fn unparsable_or_empty_yields_no_definitions() {
        assert!(extract_definitions("").is_empty());
        assert!(definition_at_line("// just a comment", 1).is_none());
    }
}
