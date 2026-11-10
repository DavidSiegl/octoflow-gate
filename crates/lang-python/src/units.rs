//! The units of a Python file that pytest runs and mutmut mutates one by one: its top-level defs and the methods of
//! its top-level classes. gate.py's `_python_units`.

use model::{Lines, Unreadable};
use tree_sitter::Node;

use crate::syntax::{decorated, end_line, fields, line, named, of_kind, undecorated, unparenthesized, Text};

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
}

/// Each [`Unit`] of the Python source `source`: the top-level defs in the order they are written, then each
/// top-level class's methods, class by class. Errs as [`crate::read`] does.
pub fn units(source: &[u8]) -> Result<Vec<Unit>, Unreadable> {
    let decoded = crate::decode::decode(source)?;
    let tree = crate::parse(&decoded)?;
    let (root, text) = (tree.root_node(), Text(&decoded));
    let mut out = block_units(root, 0, "", text);
    for class in named(root).into_iter().map(undecorated).filter(|s| s.kind() == "class_definition") {
        let name = class.child_by_field_name("name").map(|n| text.of(n)).unwrap_or_default();
        for body in fields(class, "body") {
            out.extend(block_units(body, line(class), &format!("{name}."), text));
        }
    }
    Ok(out)
}

/// The units among the statements of `block`, a module or a class's body, which the line `opener` opens, each named
/// with `prefix` before its own name.
fn block_units(block: Node, opener: u32, prefix: &str, text: Text) -> Vec<Unit> {
    let mut out = vec![];
    let mut floor = opener;
    for statement in named(block) {
        let definition = undecorated(statement);
        if definition.kind() == "function_definition" {
            let name = definition.child_by_field_name("name").map(|n| text.of(n)).unwrap_or_default();
            let decorators = of_kind(decorated(definition), "decorator").into_iter().filter_map(|d| named(d).pop());
            out.push(Unit {
                name: format!("{prefix}{name}"),
                line: line(definition),
                lines: Lines { first: line(statement), last: end_line(definition) },
                floor,
                decorators: decorators.map(|written| text.of(unparenthesized(written)).to_string()).collect(),
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
    fn a_file_that_does_not_parse_has_no_units() {
        assert_eq!(units(b"def broken(:\n    pass\n").unwrap_err().line, Some(1));
    }
}
