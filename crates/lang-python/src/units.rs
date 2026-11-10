//! The units of a Python file that pytest runs and mutmut mutates one by one: its top-level defs and the methods of
//! its top-level classes; and where a comment goes that is to cover a whole def or class, the line its header ends
//! on. gate.py's `_python_units` and `_skip_unchanged`.

use model::{Lines, Unreadable};
use tree_sitter::Node;

use crate::syntax::{children, decorated, end_line, fields, line, named, of_kind, undecorated, unparenthesized, Text};

/// A top-level def, or a method directly in a top-level class: what pytest runs as one test and mutmut mutates as one
/// function. A def under a statement such as an `if`, in a nested class or in another def is none.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unit {
    /// `f`, or `C.f` for a method.
    pub name: String,
    /// The line of its `def`, or of the `async` before it.
    pub line: u32,
    /// Its lines, from its first decorator to its end.
    pub lines: Lines,
    /// The line the statement before it in its module or class ends on, else the line of its class's `class`, else
    /// 0: the lines after it up to the unit hold nothing but the unit's own comments and blank lines.
    pub floor: u32,
    /// Each of its decorators as written, without its `@` and the parentheses around the whole of it.
    pub decorators: Vec<String>,
    /// The comment, from its `#`, on the line its header ends on, when it has one there and its body is on the lines
    /// below; None for a body on the header's own line, as `def f(): pass`, whatever comment follows that.
    pub header_comment: Option<String>,
    /// The statements directly in its body, without its docstring, in the order they are written.
    pub body: Vec<Statement>,
    /// Its lines as written, from `lines.first` to `lines.last`; only `\n` ends one, so one may end in `\r`.
    pub source: Vec<String>,
}

/// A statement directly in a [`Unit`]'s body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Statement {
    /// The line it starts on.
    pub line: u32,
    /// Whether it holds statements of its own, as an `if`, a `with`, a `match` or a def does, rather than being
    /// one simple statement.
    pub compound: bool,
}

/// The kinds of statement that hold statements of their own.
const COMPOUND: [&str; 9] = [
    "if_statement",
    "for_statement",
    "while_statement",
    "try_statement",
    "with_statement",
    "match_statement",
    "function_definition",
    "class_definition",
    "decorated_definition",
];

/// Each [`Unit`] of the Python source `source`: the top-level defs in the order they are written, then each
/// top-level class's methods, class by class. Errs as [`crate::read`] does.
pub fn units(source: &[u8]) -> Result<Vec<Unit>, Unreadable> {
    let decoded = crate::decode::decode(source)?;
    let tree = crate::parse(&decoded)?;
    let (root, text) = (tree.root_node(), Text(&decoded));
    let mut out = block_units(root, 0, "", text);
    for class in named(root).into_iter().map(undecorated).filter(|s| s.kind() == "class_definition") {
        for body in fields(class, "body") {
            out.extend(block_units(body, line(class), &format!("{}.", name(class, text)), text));
        }
    }
    Ok(out)
}

/// The lines of the Python source `source` on which the headers end of the defs and classes that hold none of the
/// units `kept` accepts, by [`Unit::name`], in no order: each top-level def it does not accept, each top-level class
/// none of whose methods it accepts, and, in a class with a method it accepts, each other def and each class
/// directly in the class's body. A comment appended to such a line stands on the header of that def or class and of
/// no other, and moves no line. A header ends on the line of the `:` before a body on the lines below, else, its
/// body being on its own line, where that line ends. Errs as [`crate::read`] does.
pub fn header_ends_outside(source: &[u8], kept: impl Fn(&str) -> bool) -> Result<Vec<u32>, Unreadable> {
    let decoded = crate::decode::decode(source)?;
    let tree = crate::parse(&decoded)?;
    let (root, text) = (tree.root_node(), Text(&decoded));
    let mut out = vec![];
    for top in definitions(root) {
        let inner: Vec<Node> = match top.kind() {
            "class_definition" => fields(top, "body").into_iter().flat_map(definitions).collect(),
            _ => vec![],
        };
        let qualified = |member: &Node| format!("{}.{}", name(top, text), name(*member, text));
        if top.kind() == "function_definition" {
            if !kept(name(top, text)) {
                out.push(header(top, text).end);
            }
        } else if inner.iter().any(|member| kept(&qualified(member))) {
            let others = inner.iter().filter(|member| !kept(&qualified(member)));
            out.extend(others.map(|member| header(*member, text).end));
        } else {
            out.push(header(top, text).end);
        }
    }
    Ok(out)
}

/// The defs and classes among the statements of `block`, a module or a class's body, each without its decorators.
fn definitions(block: Node) -> Vec<Node> {
    let statements = named(block).into_iter().map(undecorated);
    statements.filter(|s| matches!(s.kind(), "function_definition" | "class_definition")).collect()
}

/// The name of `definition`, a def or a class.
fn name<'t>(definition: Node, text: Text<'t>) -> &'t str {
    definition.child_by_field_name("name").map(|n| text.of(n)).unwrap_or_default()
}

/// Where the header of a def or a class ends.
struct Header {
    /// The line it ends on, as [`header_ends_outside`] says.
    end: u32,
    /// The comment on that line, as [`Unit::header_comment`] says.
    comment: Option<String>,
}

/// The [`Header`] of `definition`, a def or a class.
fn header(definition: Node, text: Text) -> Header {
    let body = definition.child_by_field_name("body");
    let before_body = children(definition).into_iter().take_while(|c| Some(c.id()) != body.map(|b| b.id()));
    let colon = before_body.filter(|c| c.kind() == ":").last();
    let first = body.and_then(|body| named(body).into_iter().next());
    let (Some(body), Some(colon), Some(first)) = (body, colon, first) else {
        return Header { end: end_line(definition), comment: None };
    };
    // What stands between the `:` and the body: a comment or nothing, then a line end, when the body is a block on
    // the lines below; nothing, or a backslash that continues the header's line, when the body is on that line.
    let between = text.0.get(colon.end_byte()..first.start_byte()).unwrap_or_default();
    if !between.contains('\n') || between.trim_start().starts_with('\\') {
        return Header { end: end_line(body), comment: None };
    }
    let rest = between.split('\n').next().unwrap_or_default().trim();
    Header { end: line(colon), comment: rest.starts_with('#').then(|| rest.to_string()) }
}

/// Whether `statement`, the first of a def's body, is the def's docstring, as Python's `ast.get_docstring` tells one:
/// a string alone, of one or more literals, none of them bytes or a formatted string.
fn is_docstring(statement: Node, text: Text) -> bool {
    let [written] = named(statement)[..] else { return false };
    let written = unparenthesized(written);
    let literals = match written.kind() {
        "string" => vec![written],
        "concatenated_string" => named(written),
        _ => return false,
    };
    statement.kind() == "expression_statement"
        && literals.iter().all(|literal| {
            let start = children(*literal).into_iter().find(|c| c.kind() == "string_start");
            literal.kind() == "string"
                && start.is_some_and(|start| !text.of(start).to_ascii_lowercase().contains(['b', 'f', 't']))
        })
}

/// The units among the statements of `block`, a module or a class's body, which the line `opener` opens, each named
/// with `prefix` before its own name.
fn block_units(block: Node, opener: u32, prefix: &str, text: Text) -> Vec<Unit> {
    let lines: Vec<&str> = text.0.split('\n').collect();
    let mut out = vec![];
    let mut floor = opener;
    for statement in named(block) {
        let definition = undecorated(statement);
        if definition.kind() == "function_definition" {
            let decorators = of_kind(decorated(definition), "decorator").into_iter().filter_map(|d| named(d).pop());
            let span = Lines { first: line(statement), last: end_line(definition) };
            let mut body: Vec<Node> = fields(definition, "body").into_iter().flat_map(named).collect();
            if body.first().is_some_and(|first| is_docstring(*first, text)) {
                body.remove(0);
            }
            let statement_of = |s: &Node| Statement { line: line(*s), compound: COMPOUND.contains(&s.kind()) };
            let source = lines.iter().skip(span.first as usize - 1).take((span.last + 1 - span.first) as usize);
            out.push(Unit {
                name: format!("{prefix}{}", name(definition, text)),
                line: line(definition),
                source: source.map(|line| line.to_string()).collect(),
                lines: span,
                floor,
                decorators: decorators.map(|written| text.of(unparenthesized(written)).to_string()).collect(),
                header_comment: header(definition, text).comment,
                body: body.iter().map(statement_of).collect(),
            });
        }
        floor = end_line(statement);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// (name, def line, first line, last line, floor) of each unit of `source`.
    fn placed(source: &str) -> Vec<(String, u32, u32, u32, u32)> {
        let units = units(source.as_bytes()).unwrap();
        units.into_iter().map(|u| (u.name, u.line, u.lines.first, u.lines.last, u.floor)).collect()
    }

    #[test]
    fn units_are_top_level_defs_then_the_methods_of_top_level_classes() {
        let source = "\
import os

# about f
@decorator
def f():
    def inner():
        pass
    return inner  # done
    # a comment after the body

class C:
    x = 1

    def m(self):
        pass
    class D:
        def deep(self):
            pass
    async def n(self):
        pass

if os.name:
    def conditional():
        pass
def g(): pass
";
        assert_eq!(
            placed(source),
            [
                ("f".to_string(), 5, 4, 8, 1),
                ("g".to_string(), 25, 25, 25, 24),
                ("C.m".to_string(), 14, 14, 15, 12),
                ("C.n".to_string(), 19, 19, 20, 18),
            ]
        );
    }

    #[test]
    fn a_decorated_class_s_methods_are_units_from_its_class_line() {
        let source = "@final\nclass C:\n    def m(self): pass\n\n    @staticmethod\n    def s(): pass\n";
        assert_eq!(placed(source), [("C.m".to_string(), 3, 3, 3, 2), ("C.s".to_string(), 6, 5, 6, 3)]);
    }

    #[test]
    fn decorators_are_as_written_without_the_parentheses_around_them() {
        let source = "@staticmethod\n@(classmethod)\n@pytest.mark.parametrize('x', [1])\ndef f(x): pass\n";
        let units = units(source.as_bytes()).unwrap();
        assert_eq!(units[0].decorators, ["staticmethod", "classmethod", "pytest.mark.parametrize('x', [1])"]);
    }

    #[test]
    fn a_unit_has_the_comment_where_its_header_ends_when_its_body_is_below() {
        let source = "\
def below(  # not where it ends
    a,
):  # here\r
    pass
def inline(): pass  # after the body
def continued(): \\
    pass
def bare():
    # a comment of the body
    pass
";
        let units = units(source.as_bytes()).unwrap();
        let comments: Vec<Option<&str>> = units.iter().map(|u| u.header_comment.as_deref()).collect();
        assert_eq!(comments, [Some("# here"), None, None, None]);
    }

    #[test]
    fn a_unit_s_body_is_its_statements_without_its_docstring() {
        let source = "\
def f():
    ('a doc'
     'string')
    x = 1
    if x:
        y = 2
    match x:
        case _: pass
    return x
def g():
    f'no {docstring}'
def h():
    b'bytes'
def i(): 'doc'; return 1
";
        let units = units(source.as_bytes()).unwrap();
        let bodies: Vec<Vec<(u32, bool)>> =
            units.iter().map(|u| u.body.iter().map(|s| (s.line, s.compound)).collect()).collect();
        assert_eq!(
            bodies,
            [
                vec![(4, false), (5, true), (7, true), (9, false)],
                vec![(11, false)],
                vec![(13, false)],
                vec![(14, false)]
            ]
        );
    }

    #[test]
    fn a_unit_s_source_is_its_lines_from_its_first_decorator() {
        let source = "x = 1\n@d\r\ndef f():\n    return 1\n\ny = 2\n";
        assert_eq!(units(source.as_bytes()).unwrap()[0].source, ["@d\r", "def f():", "    return 1"]);
    }

    #[test]
    fn headers_outside_the_kept_units_end_where_a_comment_covers_each() {
        let source = "\
def kept(): pass
def other(
    a,
):
    pass
@decorated
class Whole:
    def m(self): pass
class Part(
    Base,
):  # a comment
    def kept(self):
        pass
    async def other(self): return (1,
                                   2)
    class Inner:
        def deep(self): pass
    x = 1
if x:
    def conditional(): pass
";
        let kept = |name: &str| ["kept", "Part.kept"].contains(&name);
        let mut ends = header_ends_outside(source.as_bytes(), kept).unwrap();
        ends.sort_unstable();
        assert_eq!(ends, [4, 7, 15, 16]);
        assert_eq!(header_ends_outside(b"def broken(:\n", |_| false).unwrap_err().line, Some(1));
    }

    #[test]
    fn a_file_that_does_not_parse_has_no_units() {
        assert_eq!(units(b"def broken(:\n    pass\n").unwrap_err().line, Some(1));
    }
}
