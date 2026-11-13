//! A Python file's code as the duplicated-blocks check compares it: its tokens, normalised, imports left out.

use model::{token_shape, CodeToken};

use crate::syntax::{children, line, Text};

/// The tokens of the Python source `source`'s code, in the order they are written, each a [`CodeToken`]: a literal
/// stands for its kind (a string, adjacent strings and f-strings included, an integer, a float), any other name
/// tree-sitter gives a kind of its own (an identifier, `True`, `None`) for that kind, and a keyword, an operator or a
/// punctuation mark for itself. Left out are comments and `import`, `from ... import` and `from __future__ import`
/// statements. Indentation is no token.
///
/// None where `source` cannot be decoded or does not parse, as [`crate::read`] says: such a file is read by nothing
/// else either.
pub fn code_tokens(source: &[u8]) -> Vec<CodeToken> {
    let Ok(decoded) = crate::decode::decode(source) else { return vec![] };
    let Ok(tree) = crate::parse(&decoded) else { return vec![] };
    let text = Text(&decoded);
    let mut out = vec![];
    // A stack rather than recursion, as syntax::tokens walks: a sum of a few thousand terms is that deep.
    let mut pending = vec![tree.root_node()];
    while let Some(node) = pending.pop() {
        let kind = node.kind();
        if node.is_extra() || matches!(kind, "import_statement" | "import_from_statement" | "future_import_statement")
        {
            continue;
        }
        let shown = if matches!(kind, "string" | "concatenated_string" | "integer" | "float") {
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
