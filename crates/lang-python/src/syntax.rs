//! What the readers of a Python file's tree-sitter tree share: a node's text and lines, its children without the
//! comments tree-sitter leaves among them, and its tokens, which say what code does whatever its layout.

use std::ops::Range;

use tree_sitter::Node;

/// The text a tree was parsed from.
#[derive(Clone, Copy)]
pub struct Text<'t>(pub &'t str);

impl<'t> Text<'t> {
    /// `node`'s source text, as written.
    pub fn of(&self, node: Node) -> &'t str {
        self.0.get(node.byte_range()).unwrap_or_default()
    }
}

/// The line, from 1, `node` starts on.
pub fn line(node: Node) -> u32 {
    line_of(node.start_position().row)
}

/// `written`, code of one or more lines, on one line: each line break, with the spaces around it and the backslash
/// that continues a line, becomes one space, or nothing after an opening bracket or before a closing one. A finding
/// quotes a piece of code by its first line (model::first_line), which is then all of it. A line break inside a
/// string is joined as any other.
pub fn on_one_line(written: &str) -> String {
    let mut out = String::new();
    for line in written.split('\n').map(|line| line.trim().trim_end_matches('\\').trim_end()) {
        let opened = out.ends_with(['(', '[', '{']);
        if !(out.is_empty() || opened || line.starts_with([')', ']', '}'])) {
            out.push(' ');
        }
        out.push_str(line);
    }
    out
}

/// The line, from 1, of tree-sitter's row `row`, from 0.
pub fn line_of(row: usize) -> u32 {
    u32::try_from(row + 1).unwrap_or(u32::MAX)
}

/// The line, from 1, that `node`'s code ends on: its last token's, not that of a comment after it, which tree-sitter
/// puts inside a block where Python's ast ends the block before it.
pub fn end_line(node: Node) -> u32 {
    let mut at = node;
    while let Some(last) = children(at).into_iter().rev().find(|c| !is_trivia(*c)) {
        at = last;
    }
    line_of(at.end_position().row)
}

/// Whether `node` is no code: a comment, or the backslash that continues a line.
pub fn is_trivia(node: Node) -> bool {
    matches!(node.kind(), "comment" | "line_continuation")
}

/// `node`'s children, punctuation and keywords too, in source order.
pub fn children(node: Node) -> Vec<Node> {
    node.children(&mut node.walk()).collect()
}

/// `node`'s named children that are code ([`is_trivia`]), in source order.
pub fn named(node: Node) -> Vec<Node> {
    node.named_children(&mut node.walk()).filter(|c| !is_trivia(*c)).collect()
}

/// `node`'s children in its field `field`, in source order.
pub fn fields<'a>(node: Node<'a>, field: &str) -> Vec<Node<'a>> {
    node.children_by_field_name(field, &mut node.walk()).collect()
}

/// `node`'s children of the kind `kind`, in source order.
pub fn of_kind<'a>(node: Node<'a>, kind: &str) -> Vec<Node<'a>> {
    children(node).into_iter().filter(|c| c.kind() == kind).collect()
}

/// The definition `node` is or decorates: itself unless it is a `decorated_definition`.
pub fn undecorated(node: Node) -> Node {
    match node.kind() {
        "decorated_definition" => node.child_by_field_name("definition").unwrap_or(node),
        _ => node,
    }
}

/// `definition` with its decorators: the `decorated_definition` holding it, else itself.
pub fn decorated(definition: Node) -> Node {
    definition.parent().filter(|p| p.kind() == "decorated_definition").unwrap_or(definition)
}

/// `node` without the parentheses around it.
pub fn unparenthesized(mut node: Node) -> Node {
    while node.kind() == "parenthesized_expression" {
        match named(node).as_slice() {
            [inner] => node = *inner,
            _ => break,
        }
    }
    node
}

/// Where `root`'s tree first misreads the start of a format spec: the byte offset of the `=` of the first `:=`, in
/// the order the file is written, that is in a replacement field and in no brackets inside it, as in `f"{x:=^9}"`;
/// `None` when the tree holds none. Python reads the `:` there as the start of the spec, `=^9`; tree-sitter-python
/// reads an assignment expression, `x := ^9`, and what follows as its value: an error where that is no expression,
/// a name bound where it is one, a comment from a `#` on, a string from a quote on. With any other character in
/// place of that `=` it reads the spec.
///
/// A `{` opens a replacement field when it is an `interpolation`'s or a `format_expression`'s, and also, where
/// tree-sitter found an error and made neither, when a string is open at it.
///
/// Only the first is given: after it the tree may not be the file's, with or without an error in it, since such a
/// value's `#` or quote moves where the strings after it begin and end. The tree of the source without that `=`
/// says where the next one is. The first is a guess too where a later spec made an error around it: a set inside a
/// field is then taken for a field, a misreading accepted as rare and loud (docs/port-differences.md).
pub fn first_misread_spec_start(root: Node) -> Option<usize> {
    // For each bracket open at the token at hand, the outermost first, whether it opens a replacement field; and
    // how many strings are open there.
    let mut brackets: Vec<bool> = vec![];
    let mut open_strings = 0usize;
    let mut todo = vec![root];
    while let Some(node) = todo.pop() {
        match node.kind() {
            _ if node.child_count() > 0 => todo.extend(children(node).into_iter().rev()),
            "string_start" => open_strings += 1,
            "string_end" => open_strings = open_strings.saturating_sub(1),
            "{" => {
                let parent = node.parent().map(|p| p.kind()).unwrap_or_default();
                let made_a_field = matches!(parent, "interpolation" | "format_expression");
                brackets.push(made_a_field || (parent == "ERROR" && open_strings > 0));
            }
            "(" | "[" => brackets.push(false),
            ")" | "]" | "}" => {
                brackets.pop();
            }
            ":=" if brackets.last() == Some(&true) => return Some(node.start_byte() + 1),
            _ => {}
        }
    }
    None
}

/// One token of [`tokens`].
pub enum Token<'t> {
    /// A named node that holds others opens: its kind. Python's indentation is no token, so the kinds carry the
    /// structure that braces would.
    Open(&'t str),
    /// The node last opened and not yet closed ends.
    Close,
    /// A keyword, a name, an operator or punctuation, as written.
    Word(&'t str),
    /// A literal: a number, `True`, `False`, `None` or `...` as written; a string as its prefix letters, lower-case and
    /// without `u`, a quote, and its contents as written, whatever its quotes, adjacent strings of one prefix as one.
    /// Such a string with replacement fields, an f-string, is a node of the kind `string` instead: a literal of its
    /// prefix letters, a quote and its text up to the first field, then each field's tokens and each run of text
    /// after one as a literal, so that where its text is split into adjacent strings does not show either. A field's
    /// format spec is a node of the kind `format_specifier`: each run of its text as a literal, from the colon on,
    /// and the tokens of each field in it.
    Literal(String),
}

/// What [`tokens`] has still to emit: a node's tokens, or one token.
enum Item<'t> {
    Node(Node<'t>),
    Token(Token<'t>),
}

/// Gives `emit` the tokens of `root`'s code in the order they are written, comments left out, and the comma that
/// only ends a bracket's or a statement's list ([`ends_a_list`]). Two pieces of code with
/// the same tokens are the same code, but for what Python reads alike and writes differently other than quotes:
/// escapes in strings, the spelling of a number, redundant parentheses.
pub fn tokens<'t>(root: Node<'t>, text: Text<'t>, emit: &mut dyn FnMut(Token<'t>)) {
    // A stack rather than recursion: a sum of a few thousand terms is that deep.
    let mut todo = vec![Item::Node(root)];
    while let Some(item) = todo.pop() {
        let node = match item {
            Item::Token(token) => {
                emit(token);
                continue;
            }
            Item::Node(node) => node,
        };
        let strings = match node.kind() {
            "string" => vec![node],
            "concatenated_string" => named(node),
            _ => vec![],
        };
        match node.kind() {
            "comment" | "line_continuation" => {}
            "integer" | "float" | "true" | "false" | "none" | "ellipsis" => emit(Token::Literal(text.of(node).into())),
            "string_content" => emit(Token::Literal(text.of(node).into())),
            "string" | "concatenated_string" if of_one_prefix(&strings, text) => {
                todo.extend(string_items(&strings, text).into_iter().rev());
            }
            "format_specifier" => {
                todo.extend(format_spec_items(node.byte_range(), named(node), text).into_iter().rev());
            }
            "," if ends_a_list(node) => {}
            _ if node.child_count() == 0 => {
                if !node.byte_range().is_empty() {
                    emit(Token::Word(text.of(node)));
                }
            }
            kind => {
                if node.is_named() {
                    emit(Token::Open(kind));
                    todo.push(Item::Token(Token::Close));
                }
                todo.extend(children(node).into_iter().rev().map(Item::Node));
            }
        }
    }
}

/// Whether `comma`, a `,`, only ends the list it is in: nothing but comments follows it in its brackets or its
/// statement, and it is not what makes one element a tuple where the node's kind does not say so. The kind says so
/// of an expression, a `tuple` with or without the comma, and not of one index, as in `x[1,]`, nor of one target in
/// parentheses, as in `(x,) = y`, a `tuple_pattern` as `(x) = y` is.
fn ends_a_list(comma: Node) -> bool {
    let mut next = comma.next_sibling();
    while let Some(trivia) = next.filter(|n| is_trivia(*n)) {
        next = trivia.next_sibling();
    }
    let makes_a_tuple = comma.parent().is_some_and(|p| match p.kind() {
        "subscript" => fields(p, "subscript").len() == 1,
        "tuple_pattern" => named(p).len() == 1,
        _ => false,
    });
    next.is_none_or(|n| matches!(n.kind(), ")" | "]" | "}")) && !makes_a_tuple
}

/// The tokens of a replacement field's format spec, the source's text at `spec`, from its colon on, with the
/// replacement fields `fields` in it, in order ([`Token::Literal`]).
fn format_spec_items<'t>(spec: Range<usize>, fields: Vec<Node<'t>>, text: Text) -> Vec<Item<'t>> {
    let literal = |written: Range<usize>| {
        let written = text.0.get(written).filter(|w| !w.is_empty())?;
        Some(Item::Token(Token::Literal(written.to_string())))
    };
    let mut out = vec![Item::Token(Token::Open("format_specifier"))];
    let mut at = spec.start;
    for field in fields {
        out.extend(literal(at..field.start_byte()));
        out.push(Item::Node(field));
        at = field.end_byte();
    }
    out.extend(literal(at..spec.end));
    out.push(Item::Token(Token::Close));
    out
}

/// Whether `strings` is one string literal or adjacent ones that read as one: `string` nodes, at least one, all of
/// one prefix.
fn of_one_prefix(strings: &[Node], text: Text) -> bool {
    let prefix = |s: &Node| prefix(*s, text);
    !strings.is_empty()
        && strings.iter().all(|s| s.kind() == "string")
        && strings.windows(2).all(|pair| prefix(&pair[0]) == prefix(&pair[1]))
}

/// The tokens of `strings`, `string` nodes of one prefix, at least one, read as one string ([`Token::Literal`]): one
/// literal for a string without replacement fields, a node of the kind `string` for one with them.
fn string_items<'t>(strings: &[Node<'t>], text: Text) -> Vec<Item<'t>> {
    let literal = |written: String| Item::Token(Token::Literal(written));
    let mut out = vec![literal(format!("{}\"", prefix(strings[0], text)))];
    for part in strings.iter().flat_map(|s| children(*s)) {
        match (part.kind(), out.last_mut()) {
            // Text goes on from the literal before it, which may end the string before.
            ("string_content", Some(Item::Token(Token::Literal(run)))) => run.push_str(text.of(part)),
            ("string_content", _) => out.push(literal(text.of(part).into())),
            ("interpolation", _) => out.push(Item::Node(part)),
            _ => {}
        }
    }
    if out.len() > 1 {
        out.insert(0, Item::Token(Token::Open("string")));
        out.push(Item::Token(Token::Close));
    }
    out
}

/// The prefix letters of `string`, a `string` node, lower-case, without the `u` that changes nothing.
fn prefix(string: Node, text: Text) -> String {
    let start = string.child(0).map(|s| text.of(s)).unwrap_or_default();
    start.chars().filter(|c| c.is_ascii_alphabetic()).map(|c| c.to_ascii_lowercase()).filter(|&c| c != 'u').collect()
}

/// What `root`'s code does, without its comments or its layout, as 16 hexadecimal digits: equal for two pieces of
/// code with the same [`tokens`]. 64-bit FNV-1a (<http://www.isthe.com/chongo/tech/comp/fnv/>), as lang-rust's.
pub fn digest(root: Node, text: Text) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut add = |bytes: &[u8]| {
        // Each token ends with a byte no token holds, so that no two runs of tokens hash as one.
        for byte in bytes.iter().chain(&[0]) {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    };
    tokens(root, text, &mut |token| match token {
        Token::Open(kind) => {
            add(b"(");
            add(kind.as_bytes());
        }
        Token::Close => add(b")"),
        Token::Word(word) => add(word.as_bytes()),
        Token::Literal(literal) => {
            add(b"$");
            add(literal.as_bytes());
        }
    });
    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_of_several_lines_is_joined_with_no_space_inside_a_bracket() {
        let written = "self.assertEqual(\n    f(a),\n    [\n        1,\n        2\n    ],\n    'x' \\\n    'y'\n)";
        assert_eq!(on_one_line(written), "self.assertEqual(f(a), [1, 2], 'x' 'y')");
        assert_eq!(on_one_line("assert a"), "assert a");
    }
}
