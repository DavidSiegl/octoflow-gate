//! The gate's Python front end: a Python source file, read with tree-sitter-python, as the model the parser checks
//! judge (crates/model). docs/port-differences.md lists where it reads a file differently from the gate up to 0.6.0,
//! which read Python with Python's own `ast`.

use std::borrow::Cow;
use std::collections::{BTreeSet, HashMap};
use std::path::Path;

use model::{Binding, Function, Lines, SourceFile, Unreadable};
use tree_sitter::{Node, Parser, Tree};

mod code_tokens;
mod binds;
mod decode;
mod flow;
mod imports;
mod syntax;
mod testing;
mod units;

use binds::{binds, type_parameters, How};
pub use code_tokens::code_tokens;
pub use imports::imports;
pub use units::{header_ends_outside, units, Statement, Unit};

use syntax::{children, decorated, digest, end_line, fields, line, line_of, named, Text};

/// The Python source `source`, the file at `path` from the tree's top level, as the model has it. `among_tests` says
/// whether the project places the file among its tests (gate.toml's `test-files.python`), which makes the whole of
/// it test code; a file elsewhere has no tests. `path` only names the module, for a test that reads one of its
/// constants as `module.NAME`.
///
/// What the model's parts are in Python:
/// - a [`Function`] is a def no other def holds, at module level, in a class at any depth or under a module-level
///   statement such as an `if`, named by its classes as `C.D.f`; none is exempt, since Python's test code is told by
///   its file. Its calls resolve by name through Python's scopes ([`flow::callees`]), and its flow is as
///   [`flow::flow`] says;
/// - a [`model::Test`] is a module-level def named `test*` or such a method directly in a module-level class, its
///   assertions the `assert` statements, `self.assert*(...)` and `pytest.raises(...)` calls anywhere in it, and a
///   [`model::TestGroup`] a module-level class that holds such methods or is named `Test*`;
/// - a weakening is a name that skips a test or expects it to fail, as `skipIf` or `pytest.mark.xfail`, or an
///   `except` that catches everything and does nothing;
/// - a binding is a name bound by an assignment (to a name or to an attribute of `self` or `cls`), as a parameter, by
///   a def or class, a loop, a with's or an except's `as`, an import, a match capture or a type parameter, the name
///   and parameters of a def marked `@override` fixed by its base class;
/// - a constant is a name one module-level assignment binds to an expression that calls nothing.
///
/// Errs when `source` cannot be decoded as its byte order mark, its coding cookie or UTF-8 says, about the whole
/// file; or when it does not parse as Python, at the line of the first token tree-sitter could not place.
pub fn read(source: &[u8], path: &Path, among_tests: bool) -> Result<SourceFile, Unreadable> {
    let decoded = decode::decode(source)?;
    let tree = parse(&decoded)?;
    let (root, text) = (tree.root_node(), Text(&decoded));
    let mut file =
        SourceFile { functions: functions(root, text), bindings: bindings(root, text), ..SourceFile::default() };
    if among_tests {
        let spellings = testing::module_spellings(root, path, text);
        (file.tests, file.test_groups) = testing::tests(root, &spellings, text);
        let last = u32::try_from(decoded.bytes().filter(|&b| b == b'\n').count() + 1).unwrap_or(u32::MAX);
        file.test_lines = vec![Lines { first: 1, last }];
        file.weakenings = testing::weakenings(root, text);
        file.constants = testing::constants(root, text);
    }
    Ok(file)
}

/// How much of a file's first line [`is_python`] reads, in bytes: enough for any `#!` line.
pub const FIRST_LINE_BYTES: usize = 256;

/// Whether the file at `path` holds Python source: its name ends in `.py`, or its first line is a `#!` naming python.
/// A `#![` is no `#!` line but a Rust inner attribute, as `#![cfg(feature = "python")]`.
/// `head` gives the file's first bytes, [`FIRST_LINE_BYTES`] or more of them when it has as many, and none for a file
/// that cannot be read; it is called only when the name does not decide.
pub fn is_python(path: &[u8], head: impl FnOnce() -> Vec<u8>) -> bool {
    if path.ends_with(b".py") {
        return true;
    }
    let head = head();
    let first = head[..head.len().min(FIRST_LINE_BYTES)].split(|&b| b == b'\n').next().unwrap_or_default();
    first.starts_with(b"#!") && !first.starts_with(b"#![") && first.windows(b"python".len()).any(|w| w == b"python")
}

/// The line, from 1, and the text of each comment of the Python source `source`, in the order they are written; None
/// when it cannot be decoded or does not parse, and so has no comments that can be told from its strings.
pub fn comments(source: &[u8]) -> Option<Vec<(u32, String)>> {
    let decoded = decode::decode(source).ok()?;
    let tree = parse(&decoded).ok()?;
    let mut out = vec![];
    let mut todo = vec![tree.root_node()];
    while let Some(node) = todo.pop() {
        if node.kind() == "comment" {
            out.push((line(node), Text(&decoded).of(node).to_string()));
        }
        todo.extend(children(node).into_iter().rev());
    }
    Some(out)
}

/// The tree of the Python source `text`, each node at the bytes of `text` it was read from. Errs when it does not
/// parse, as [`read`] says.
fn parse(text: &str) -> Result<Tree, Unreadable> {
    let no_tree = || Unreadable { line: None, why: "could not be parsed: tree-sitter gave no tree".into() };
    let mut parser = Parser::new();
    parser.set_language(&tree_sitter_python::LANGUAGE.into()).map_err(|_| no_tree())?;
    // What tree-sitter parses: `text`, but for a space in place of the `=` that starts a format spec it would
    // misread ([`syntax::first_misread_spec_start`]). A spec's text is anything, so the tree is that of `text` with
    // those specs read as Python reads them, and what a spec says is read from `text`. Each round takes one `:=`
    // out, so the loop ends; a file is parsed once more for each such spec it holds.
    let mut parsed = Cow::Borrowed(text.as_bytes());
    let tree = loop {
        let tree = parser.parse(&*parsed, None).ok_or_else(no_tree)?;
        match syntax::first_misread_spec_start(tree.root_node()) {
            Some(at) => parsed.to_mut()[at] = b' ',
            None => break tree,
        }
    };
    match first_error(tree.root_node()) {
        Some(error) => {
            Err(Unreadable { line: Some(line_of(error.start_position().row)), why: "does not parse as Python".into() })
        }
        None => Ok(tree),
    }
}

/// The first node, in the order the file is written, that tree-sitter could not parse: an error, or a token it took
/// to be missing.
fn first_error(root: Node) -> Option<Node> {
    if !root.has_error() {
        return None;
    }
    let mut at = root;
    loop {
        if at.is_error() || at.is_missing() {
            return Some(at);
        }
        at = children(at).into_iter().find(|c| c.has_error() || c.is_missing())?;
    }
}

/// The kinds of node whose statements may hold a def the check judges as one, beside a module and a class.
const STATEMENT_HOLDERS: [&str; 14] = [
    "block",
    "if_statement",
    "elif_clause",
    "else_clause",
    "for_statement",
    "while_statement",
    "try_statement",
    "except_clause",
    "except_group_clause",
    "finally_clause",
    "with_statement",
    "match_statement",
    "case_clause",
    "decorated_definition",
];

/// Each def of the module `root` that the cognitive-complexity check judges as one ([`read`]), in the order they
/// start.
fn functions(root: Node, text: Text) -> Vec<Function> {
    struct Found<'a> {
        name: String,
        definition: Node<'a>,
        /// The qualified name of the class it is a method of, and the type parameters of the classes around it.
        class: Option<String>,
        typed: BTreeSet<String>,
    }
    let name = |definition: Node| definition.child_by_field_name("name").map(|n| text.of(n)).unwrap_or_default();
    let mut found = vec![];
    // The nodes whose statements are still to be read, each with the qualified-name prefix there and what a def
    // there is found with.
    let mut todo = vec![(root, String::new(), None::<String>, BTreeSet::new())];
    while let Some((node, prefix, class, typed)) = todo.pop() {
        for child in named(node) {
            match child.kind() {
                "function_definition" => {
                    let name = format!("{prefix}{}", name(child));
                    found.push(Found { name, definition: child, class: class.clone(), typed: typed.clone() });
                }
                "class_definition" => {
                    let qualified = format!("{prefix}{}", name(child));
                    let mut typed = typed.clone();
                    typed.extend(type_parameters(child, text).into_iter().map(|b| b.name.to_string()));
                    for body in fields(child, "body") {
                        todo.push((body, format!("{qualified}."), Some(qualified.clone()), typed.clone()));
                    }
                }
                kind if STATEMENT_HOLDERS.contains(&kind) => {
                    todo.push((child, prefix.clone(), class.clone(), typed.clone()));
                }
                _ => {}
            }
        }
    }
    found.sort_by_key(|f| f.definition.start_byte());
    let mut by_key: HashMap<flow::Callee, Vec<usize>> = HashMap::new();
    for (i, f) in found.iter().enumerate() {
        by_key.entry((f.class.clone(), name(f.definition).to_string())).or_default().push(i);
    }
    found
        .iter()
        .map(|f| {
            let callees = flow::callees(f.definition, f.class.as_deref(), &f.typed, text);
            let calls: BTreeSet<usize> = callees.iter().filter_map(|key| by_key.get(key)).flatten().copied().collect();
            let body = fields(f.definition, "body");
            Function {
                name: f.name.clone(),
                line: line(f.definition),
                lines: Lines { first: line(decorated(f.definition)), last: end_line(f.definition) },
                body: body.first().map(|body| digest(*body, text)).unwrap_or_default(),
                exempt: false,
                calls: calls.into_iter().collect(),
                flow: flow::flow(body, None, text),
            }
        })
        .collect()
}

/// Every name the module `root` binds ([`read`]), by line and column.
fn bindings(root: Node, text: Text) -> Vec<Binding> {
    let mut out = vec![];
    let mut todo = vec![root];
    while let Some(node) = todo.pop() {
        let overrides = node.kind() == "function_definition" && marked_override(node, text);
        for bound in binds(node, text) {
            out.push(Binding {
                name: bound.name.to_string(),
                line: line_of(bound.at.row),
                column: u32::try_from(bound.at.column).unwrap_or(u32::MAX),
                fixed: overrides && matches!(bound.how, How::Name | How::Parameter),
            });
        }
        todo.extend(named(node));
    }
    out.sort_by_key(|b| (b.line, b.column));
    out
}

/// Whether `definition`, a def, has a decorator named `override`, typing's or any other, as PEP 698 marks a method
/// whose name and signature its base class fixes.
fn marked_override(definition: Node, text: Text) -> bool {
    let decorators = syntax::of_kind(decorated(definition), "decorator");
    decorators.into_iter().filter_map(|d| named(d).into_iter().next()).any(|written| {
        let name = match written.kind() {
            "attribute" => written.child_by_field_name("attribute"),
            "identifier" => Some(written),
            _ => None,
        };
        name.is_some_and(|n| text.of(n) == "override")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn python_is_a_py_name_or_a_first_line_that_names_python() {
        assert!(is_python(b"a/b.py", || panic!("the name decides")));
        assert!(is_python(b"bin/tool", || b"#!/usr/bin/env python3\nprint(1)\n".to_vec()));
        assert!(!is_python(b"src/python.rs", || b"#![cfg(feature = \"python\")]\n".to_vec()));
        for head in [&b"#!/bin/sh\n# python\n"[..], b"# python\n", b"", b"import python\n"] {
            assert!(!is_python(b"bin/tool", || head.to_vec()), "{head:?}");
        }
        // Only the first 256 bytes of the first line are read.
        let late = [b"#!".to_vec(), vec![b' '; 256], b"python\n".to_vec()].concat();
        assert!(!is_python(b"bin/tool", || late));
    }

    #[test]
    fn comments_are_told_from_strings_and_none_are_read_of_a_file_that_does_not_parse() {
        let source = b"x = '# noqa'  # one\n\ndef f():\n    # two\n    return 1\n";
        assert_eq!(comments(source), Some(vec![(1, "# one".to_string()), (4, "# two".to_string())]));
        assert_eq!(comments(b"def broken(:\n    pass  # noqa\n"), None);
        assert_eq!(comments(b"x = '\xff'  # noqa\n"), None);
        // The `#` of a format spec starts no comment.
        assert_eq!(comments(b"x = f'{y:=#x}'  # one\n"), Some(vec![(1, "# one".to_string())]));
    }

    #[test]
    fn a_file_that_does_not_parse_is_unreadable_at_its_first_error() {
        let e = read(b"x = 1\n\ndef broken(:\n    pass\n", Path::new("a.py"), false).unwrap_err();
        assert_eq!((e.line, e.why.as_str()), (Some(3), "does not parse as Python"));
        // A format spec that starts with `=`, which tree-sitter misreads, hides no error after it.
        for spec in ["=^9", "=#x", "=+10,.2f"] {
            let source = format!("def f(x):\n    return f\"{{x:{spec}}}\"\n\n\ndef broken(:\n    pass\n");
            let e = read(source.as_bytes(), Path::new("a.py"), false).unwrap_err();
            assert_eq!((e.line, e.why.as_str()), (Some(5), "does not parse as Python"), "{spec}");
        }
    }
}
