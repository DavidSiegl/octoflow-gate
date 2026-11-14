//! The gate's Go front end: a Go source file, read with tree-sitter-go, as the model the parser checks judge
//! (crates/model), which files of a tree are Go source the go tool builds, and which of those a generator wrote.

use std::collections::{BTreeSet, HashMap};

use model::{first_line, Function, Lines, SourceFile, Unreadable};
use tree_sitter::{Node, Parser};

mod code_tokens;
mod binds;
mod constraint;
mod flow;
mod syntax;
mod testing;

pub use code_tokens::code_tokens;
pub use constraint::platform_suffix;
use flow::Call;
use syntax::{doc_line, end_line, function_name, line, named, receiver_name, receiver_type, Text};

/// Whether the file at `path`, from the tree's top level, is Go source that the go tool builds into one of the
/// module's own packages: a `.go` file whose name and directories do not start with `.` or `_` and none of whose
/// directories is `testdata`, which the go tool leaves out of a package, or `vendor`, whose packages `./...` does not
/// match, as they are the module's dependencies (`go help packages`). Whether it lies in a Go module, which only the
/// tree can tell, is the caller's to say, and whether it is [`generated`], which only its text can.
pub fn is_go(path: &[u8]) -> bool {
    let left_out =
        |part: &[u8]| part.starts_with(b".") || part.starts_with(b"_") || part == b"testdata" || part == b"vendor";
    path.ends_with(b".go") && !path.split(|&b| b == b'/').any(left_out)
}

/// Whether the Go source `source` says a tool generated it, by the header <https://go.dev/s/generatedcode> fixes: a
/// line, before the first that starts the `package` clause, that is `// Code generated ` and ` DO NOT EDIT.` with any
/// text between, a `\r` before its `\n` allowed. Such a file is the generator's output, not code written to be
/// judged.
pub fn generated(source: &[u8]) -> bool {
    const START: &[u8] = b"// Code generated ";
    const END: &[u8] = b" DO NOT EDIT.";
    let package = |line: &[u8]| {
        line.strip_prefix(b"package").is_some_and(|rest| matches!(rest.first(), None | Some(b' ' | b'\t')))
    };
    let lines = source.split(|&b| b == b'\n').map(|line| line.strip_suffix(b"\r").unwrap_or(line));
    lines
        .take_while(|line| !package(line))
        .any(|line| line.len() >= START.len() + END.len() && line.starts_with(START) && line.ends_with(END))
}

/// Whether the Go file at `path` is test code whole: a `_test.go` file, which only `go test` builds.
pub fn among_tests(path: &[u8]) -> bool {
    path.ends_with(b"_test.go")
}

/// The Go source `source`, as the model has it. `among_tests` says whether it is a `_test.go` file ([`among_tests`]),
/// which makes the whole of it test code; Go has no test code inside other files.
///
/// What the model's parts are in Go:
/// - a [`Function`] is a function or method declared with a body, named `f` or, for a method, by its receiver's type,
///   `T.m`, with the `func` literals it holds part of its flow (flow.rs says how each construct scores); its lines
///   start at its doc comment; its calls resolve within the file as [`calls`] says;
/// - the tests, their assertions and the weakenings are those testing.rs's `read` finds, in a `_test.go` file only;
/// - a binding is one of those binds.rs's `bindings` lists;
/// - there are no test groups, as Go has no item that holds tests;
/// - the constants are those testing.rs's `constants` gives.
///
/// Errs when `source` is not UTF-8, or does not parse as Go, at the line of the first token tree-sitter could not
/// place.
pub fn read(source: &[u8], among_tests: bool) -> Result<SourceFile, Unreadable> {
    let (tree, text) = parse(source)?;
    let root = tree.root_node();
    let n = u32::try_from(text.0.split('\n').count()).unwrap_or(u32::MAX);
    let (tests, weakenings) = if among_tests { testing::read(root, text) } else { (vec![], vec![]) };
    Ok(SourceFile {
        functions: functions(root, text, among_tests),
        tests,
        test_groups: vec![],
        test_lines: if among_tests { vec![Lines { first: 1, last: n }] } else { vec![] },
        weakenings,
        bindings: binds::bindings(root, text),
        constants: testing::constants(root, text),
    })
}

/// Each function and method with a body that the Go source `source` declares, in the order they start, by its name as
/// [`read`] gives it, with a digest of its declaration but its name: its receiver, type parameters, parameters,
/// results and body, whatever their layout and comments. Two declarations of one digest differ at most in their names,
/// comments and layout, so a function whose digest its base shares under its name was edited in none of its code;
/// unlike [`model::Function::body`], it changes with the signature. Errs as [`read`] does.
pub fn declarations(source: &[u8]) -> Result<Vec<(String, String)>, Unreadable> {
    let (tree, text) = parse(source)?;
    let declared = named(tree.root_node()).into_iter().filter(|n| {
        matches!(n.kind(), "function_declaration" | "method_declaration") && n.child_by_field_name("body").is_some()
    });
    Ok(declared.map(|node| (function_name(node, text), syntax::declaration_digest(node, text))).collect())
}

/// `source` parsed as Go, with its text; errs as [`read`] does.
fn parse(source: &[u8]) -> Result<(tree_sitter::Tree, Text<'_>), Unreadable> {
    let text = std::str::from_utf8(source).map_err(|e| Unreadable { line: None, why: format!("is not UTF-8: {e}") })?;
    let mut parser = Parser::new();
    let parsed = parser.set_language(&tree_sitter_go::LANGUAGE.into()).ok().and_then(|()| parser.parse(text, None));
    let Some(tree) = parsed else {
        return Err(Unreadable { line: None, why: "could not be parsed: tree-sitter gave no tree".into() });
    };
    let root = tree.root_node();
    let text = Text(text);
    if let Some(error) = first_error(root) {
        let what = if error.is_missing() {
            format!("expected `{}`", error.kind())
        } else {
            let token = syntax::tokens(error, text).into_iter().next().unwrap_or_default();
            format!("unexpected `{}`", first_line(token))
        };
        return Err(Unreadable { line: Some(line(error)), why: format!("does not parse as Go ({what})") });
    }
    Ok((tree, text))
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
        let mut cursor = at.walk();
        at = at.children(&mut cursor).find(|c| c.has_error() || c.is_missing())?;
    }
}

/// The functions and methods with a body that `root` declares, in the order they start, each test code when
/// `among_tests`.
fn functions(root: Node, text: Text, among_tests: bool) -> Vec<Function> {
    // Each function, with the type of its receiver, None for a function, and the calls its body makes.
    let mut read: Vec<(Function, Option<String>, Vec<Call>)> = vec![];
    for node in named(root).into_iter().filter(|n| matches!(n.kind(), "function_declaration" | "method_declaration")) {
        let Some(body) = node.child_by_field_name("body") else { continue };
        let receiver = node.child_by_field_name("receiver");
        let (flow, calls) = flow::flow(body, receiver.and_then(|r| receiver_name(r, text)), text);
        let function = Function {
            name: function_name(node, text),
            line: line(node),
            lines: Lines { first: doc_line(node).unwrap_or(line(node)), last: end_line(node) },
            body: syntax::digest(body, text),
            exempt: among_tests,
            calls: vec![],
            flow,
        };
        read.push((function, receiver.and_then(|r| receiver_type(r, text)), calls));
    }
    let callers: Vec<_> =
        read.iter().map(|(f, ty, calls)| (f.name.as_str(), ty.as_deref(), calls.as_slice())).collect();
    let resolved = calls(&callers);
    read.into_iter().zip(resolved).map(|((function, _, _), calls)| Function { calls, ..function }).collect()
}

/// For each of `functions`, (its name as [`syntax::function_name`] gives it, the type of its receiver, None for a
/// function, and the calls its body makes), the indexes in `functions` of the functions its calls may reach,
/// ascending.
///
/// A call resolves by name within the file: `f(...)` to each function named f, and `r.m(...)`, `r` being the calling
/// method's receiver, to each method m of the caller's receiver type. Any other call, as `pkg.F(...)`, `x.m(...)` on
/// another variable or a call through a function value, resolves to none; and a local variable or parameter that
/// shadows a function's name is not told from it, so a call of it reaches the function.
fn calls(functions: &[(&str, Option<&str>, &[Call])]) -> Vec<Vec<usize>> {
    let mut by_key: HashMap<(Option<&str>, &str), Vec<usize>> = HashMap::new();
    for (i, (name, ty, _)) in functions.iter().enumerate() {
        let own = name.rsplit('.').next().unwrap_or_default();
        by_key.entry((*ty, own)).or_default().push(i);
    }
    let reach = |ty: Option<&str>, calls: &[Call]| -> Vec<usize> {
        let keys = calls.iter().map(|call| match call {
            Call::Plain(name) => (None, name.as_str()),
            Call::OwnMethod(name) => (ty, name.as_str()),
        });
        let reached: BTreeSet<usize> = keys.filter_map(|key| by_key.get(&key)).flatten().copied().collect();
        reached.into_iter().collect()
    };
    functions.iter().map(|(_, ty, calls)| reach(*ty, calls)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn go_source_is_a_go_file_outside_what_the_go_tool_leaves_out() {
        for path in ["main.go", "cmd/tool/main.go", "pkg/x_test.go", "vendored/a.go", "pkg/vendor.go"] {
            assert!(is_go(path.as_bytes()), "{path}");
        }
        for path in [
            "main.rs",
            "pkg/testdata/broken.go",
            "_examples/a.go",
            "pkg/.hidden/a.go",
            "pkg/_a.go",
            "go.mod",
            "vendor/example.com/dep/a.go",
            "svc/vendor/example.com/dep/a_test.go",
        ] {
            assert!(!is_go(path.as_bytes()), "{path}");
        }
        assert!(among_tests(b"pkg/x_test.go") && !among_tests(b"pkg/x.go"));
    }

    #[test]
    fn a_file_is_generated_by_the_header_go_reads_before_its_package_clause() {
        for source in [
            "// Code generated by protoc-gen-go. DO NOT EDIT.\n\npackage api\n",
            "// Copyright 2026 The Authors.\n// SPDX-License-Identifier: MIT\n\n\
             // Code generated by stringer -type=Kind; DO NOT EDIT.\r\n\npackage kind\n",
            "//go:build linux\n\n// Code generated x DO NOT EDIT.\npackage x\n",
        ] {
            assert!(generated(source.as_bytes()), "{source:?}");
        }
        for source in [
            "package api\n\n// Code generated by protoc-gen-go. DO NOT EDIT.\n",
            "// Code generated by hand. Edit freely.\npackage api\n",
            "//Code generated by x. DO NOT EDIT.\npackage api\n",
            "// Code generated by x. DO NOT EDIT. Really.\npackage api\n",
            "// Code generated DO NOT EDIT.\npackage api\n",
            "package api\n",
            "",
        ] {
            assert!(!generated(source.as_bytes()), "{source:?}");
        }
    }
}
