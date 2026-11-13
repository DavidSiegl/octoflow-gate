//! A Go file's code as the duplicated-blocks check compares it: its tokens, normalised, imports left out.

use model::{token_shape, CodeToken};

use crate::syntax::{children, line};

/// The tokens of the Go source `source`'s code, in the order they are written, each a [`CodeToken`]: a literal of a
/// basic type stands for its kind (an interpreted or raw string, a rune, an integer, a float, an imaginary number),
/// any other name tree-sitter gives a kind of its own (an identifier, a type's, a field's or a package's name, `nil`,
/// `true`) for that kind, and a keyword, an operator or a punctuation mark for itself. A composite or function literal
/// is code like any other. Left out are comments, the `package` clause and `import` declarations, a file's header.
///
/// None where `source` is not UTF-8 or does not parse as Go, as [`crate::read`] says: such a file is read by nothing
/// else either.
pub fn code_tokens(source: &[u8]) -> Vec<CodeToken> {
    let Ok((tree, text)) = crate::parse(source) else { return vec![] };
    let mut out = vec![];
    // A stack rather than recursion, as syntax::tokens walks.
    let mut pending = vec![tree.root_node()];
    while let Some(node) = pending.pop() {
        let kind = node.kind();
        if node.is_extra() || matches!(kind, "package_clause" | "import_declaration") {
            continue;
        }
        let literal = kind.ends_with("_literal") && !matches!(kind, "composite_literal" | "func_literal");
        let shown = if literal {
            format!("${kind}")
        } else if node.child_count() > 0 {
            pending.extend(children(node).into_iter().rev());
            continue;
        } else if node.is_named() {
            format!("${kind}")
        } else {
            text.of(node).to_string()
        };
        out.push(CodeToken { shape: token_shape(&shown), line: line(node) });
    }
    out
}
