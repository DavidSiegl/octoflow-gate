//! A Rust file's code as the duplicated-blocks check compares it: its tokens, normalised, test code and imports left
//! out.

use model::{token_shape, CodeToken};
use tree_sitter::{Node, Parser};

use crate::syntax::{children, holds_only_under_test, named, Text};

/// The tokens of the Rust source `source`'s code, in the order they are written, each a [`CodeToken`]: a literal
/// stands for its kind (a string, a number, a character, a boolean), any other name tree-sitter gives a kind of its
/// own (an identifier, a type's name, a field's, a primitive type, a lifetime's name) for that kind, and a keyword or
/// a punctuation mark for itself. Left out are comments, `use` and `extern crate` declarations, and test code: every
/// item marked `#[cfg(test)]` (or an `all` that needs `test`), its attributes with it, and the whole of a module or
/// file that opens with `#![cfg(test)]`. A macro's arguments are tokens as written.
///
/// None where `source` is not UTF-8 or does not parse as Rust: such a file is read by nothing else either.
pub fn code_tokens(source: &[u8]) -> Vec<CodeToken> {
    let mut parser = Parser::new();
    let parsed = std::str::from_utf8(source)
        .ok()
        .and_then(|text| parser.set_language(&tree_sitter_rust::LANGUAGE.into()).ok().map(|()| text))
        .and_then(|text| parser.parse(text, None));
    let Some(tree) = parsed.filter(|tree| !tree.root_node().has_error()) else { return vec![] };
    let text = Text::of_file(source);
    let mut out = vec![];
    // A stack rather than recursion, as syntax::tokens walks: an expression a few thousand terms long is that deep.
    let mut pending = vec![tree.root_node()];
    while let Some(node) = pending.pop() {
        if node.is_extra() || matches!(node.kind(), "use_declaration" | "extern_crate_declaration") {
            continue;
        }
        let shown = if node.kind().ends_with("_literal") {
            format!("${}", node.kind())
        } else if node.child_count() > 0 {
            if !opens_with_test_only(node, text) {
                pending.extend(outside_tests(node, text).into_iter().rev());
            }
            continue;
        } else if node.is_named() {
            format!("${}", node.kind())
        } else {
            text.of(node).to_string()
        };
        let line = u32::try_from(node.start_position().row + 1).unwrap_or(u32::MAX);
        out.push(CodeToken { shape: token_shape(&shown), line });
    }
    out
}

/// `node`'s children, but for each run of attributes that holds an attribute true only under `cfg(test)`, and the
/// item after it: tree-sitter makes an item's outer attributes its siblings.
fn outside_tests<'n>(node: Node<'n>, text: Text) -> Vec<Node<'n>> {
    let mut out = vec![];
    let mut attributes = vec![];
    let mut marked = false;
    for child in children(node) {
        if child.kind() == "attribute_item" {
            marked |= holds_only_under_test(child, text);
            attributes.push(child);
        } else if child.is_extra() {
            out.push(child);
        } else {
            if !marked {
                out.append(&mut attributes);
                out.push(child);
            }
            attributes.clear();
            marked = false;
        }
    }
    out.extend(attributes);
    out
}

/// Whether `node` is a file or a module's body that opens with `#![cfg(test)]`, making the whole of it test code.
fn opens_with_test_only(node: Node, text: Text) -> bool {
    matches!(node.kind(), "source_file" | "declaration_list")
        && named(node).into_iter().any(|n| n.kind() == "inner_attribute_item" && holds_only_under_test(n, text))
}
