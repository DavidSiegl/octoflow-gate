//! What the passes over a Go file's tree-sitter tree share: the text of a node, its lines, its tokens and a digest of
//! them, and the shapes of Go that more than one pass reads.

use tree_sitter::Node;

/// The source a tree was parsed from, UTF-8.
#[derive(Clone, Copy)]
pub struct Text<'t>(pub &'t str);

impl<'t> Text<'t> {
    /// `node`'s source text, as written.
    pub fn of(self, node: Node) -> &'t str {
        &self.0[node.byte_range()]
    }
}

/// The line, from 1, that `node` starts on.
pub fn line(node: Node) -> u32 {
    u32::try_from(node.start_position().row + 1).unwrap_or(u32::MAX)
}

/// The line, from 1, of `node`'s last byte.
pub fn end_line(node: Node) -> u32 {
    let end = node.end_position();
    let row = if end.column == 0 && end.row > 0 && node.end_byte() > node.start_byte() { end.row - 1 } else { end.row };
    u32::try_from(row + 1).unwrap_or(u32::MAX)
}

/// `node`'s children, named and anonymous alike (punctuation and keywords too), in source order.
pub fn children(node: Node) -> Vec<Node> {
    node.children(&mut node.walk()).collect()
}

/// `node`'s named children, comments included, in source order.
pub fn named(node: Node) -> Vec<Node> {
    node.named_children(&mut node.walk()).collect()
}

/// The children of `node` under `field`, in source order: a parameter declaration's several names, say.
pub fn fields<'a>(node: Node<'a>, field: &str) -> Vec<Node<'a>> {
    node.children_by_field_name(field, &mut node.walk()).collect()
}

/// Whether `node` is one literal token of Go: a number, a rune or a string. `true`, `false`, `nil` and `iota` are
/// predeclared names, not literals.
pub fn is_literal(node: Node) -> bool {
    matches!(
        node.kind(),
        "int_literal"
            | "float_literal"
            | "imaginary_literal"
            | "rune_literal"
            | "interpreted_string_literal"
            | "raw_string_literal"
    )
}

/// The tokens of `node` as written, in order, comments left out and each literal one token.
pub fn tokens<'t>(node: Node, text: Text<'t>) -> Vec<&'t str> {
    let mut out = vec![];
    // Nodes still to read, the next one last.
    let mut pending = vec![node];
    while let Some(node) = pending.pop() {
        if node.kind() == "comment" {
            continue;
        }
        if node.child_count() == 0 || is_literal(node) {
            let token = text.of(node);
            // tree-sitter-go's grammar may end a statement with a newline token, which is layout, not code.
            if !token.trim().is_empty() {
                out.push(token);
            }
            continue;
        }
        pending.extend(children(node).into_iter().rev());
    }
    out
}

/// A digest of `node`'s [`tokens`]: equal for two nodes of the same code whatever their layout and comments. 64-bit
/// FNV-1a (<http://www.isthe.com/chongo/tech/comp/fnv/>), as lang-rust's, as 16 hexadecimal digits.
pub fn digest(node: Node, text: Text) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in tokens(node, text).join(" ").bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// The name of the type a method's `receiver`, its parameter list, names, without a pointer, type arguments or
/// parentheses: `T` for `(r *T[K])`. None for a receiver list that holds no type, which does not compile.
pub fn receiver_type(receiver: Node, text: Text) -> Option<String> {
    let declaration = named(receiver).into_iter().find(|p| p.kind() == "parameter_declaration")?;
    let mut ty = declaration.child_by_field_name("type")?;
    loop {
        ty = match ty.kind() {
            "pointer_type" | "parenthesized_type" => ty.named_child(0)?,
            "generic_type" => ty.child_by_field_name("type")?,
            "type_identifier" => return Some(text.of(ty).into()),
            _ => return None,
        };
    }
}

/// The name of the receiver variable of a method, `r` in `(r *T)`; None where the method names none, as in `(T)` or
/// `(_ T)`.
pub fn receiver_name<'t>(receiver: Node, text: Text<'t>) -> Option<&'t str> {
    let declaration = named(receiver).into_iter().find(|p| p.kind() == "parameter_declaration")?;
    let name = text.of(declaration.child_by_field_name("name")?);
    (name != "_").then_some(name)
}

/// The line the doc comment of the declaration `node` starts on, the comments right above it with no blank line
/// between; None where it has none.
pub fn doc_line(node: Node) -> Option<u32> {
    let mut first = None;
    let mut below = line(node);
    let mut at = node.prev_sibling();
    while let Some(comment) = at.filter(|c| c.kind() == "comment" && end_line(*c) + 1 == below) {
        below = line(comment);
        first = Some(below);
        at = comment.prev_sibling();
    }
    first
}

/// The name of the function or method the declaration `node` declares, a method's qualified by its receiver's type, as
/// `go doc` writes it: `Add`, `Stack.Push` for `func (s *Stack[T]) Push(...)`.
pub fn function_name(node: Node, text: Text) -> String {
    let name = node.child_by_field_name("name").map(|n| text.of(n)).unwrap_or_default();
    match node.child_by_field_name("receiver").and_then(|r| receiver_type(r, text)) {
        Some(ty) => format!("{ty}.{name}"),
        None => name.into(),
    }
}
