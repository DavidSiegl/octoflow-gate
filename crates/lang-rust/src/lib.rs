//! The gate's Rust front end: a Rust source file, read with tree-sitter-rust, as the model the parser checks judge
//! (crates/model). docs/port-differences.md lists where it reads a file differently from the gate up to 0.6.0, which
//! read Rust with syn.

use std::collections::{BTreeSet, HashMap};

use model::{first_line, Function, Lines, SourceFile, Test, TestGroup, Unreadable, Weakening, WeakeningKind};
use tree_sitter::{Node, Parser};

mod code_tokens;
mod imports;
mod skips;
mod syntax;

pub use code_tokens::code_tokens;
pub use imports::imports;

use syntax::{Fn, Holder, Items, Text};

/// The Rust source `source`, as the model has it. `among_tests` says whether the project places the file among its
/// tests (gate.toml's `test-files.rust`), which makes the whole of it test code; otherwise its test code is its inline
/// modules marked `#[cfg(test)]`.
///
/// What the model's parts are in Rust:
/// - a [`Function`] is a `fn` with a body that no other `fn` holds, in an `impl`, a `trait` or an inline module or
///   not, named by them as `engine::Workflow<'a>::poll` or `<Point as fmt::Display>::fmt`; its calls resolve by path
///   within the file as [`calls`] says;
/// - a [`Test`] is a `fn` among the test code with an attribute whose path ends in `test`, and its assertions the
///   assertion macros its body calls;
/// - a [`TestGroup`] is an inline module;
/// - a weakening is a skipping attribute (skips.rs), a `#[should_panic]` without `expected` on a test, or a value
///   thrown away by `let _ =`, `_ =` or `.ok()` in a `fn` among the test code that holds an assertion it calls or a
///   `catch_unwind`;
/// - a binding is a name a `fn`, a parameter, a pattern, an item or a `use ... as` binds, the name and parameters of a
///   `fn` of an `impl Trait for T` fixed by the trait.
///
/// Errs when `source` is not UTF-8, or does not parse as Rust, at the line of the first token tree-sitter could not
/// place.
pub fn read(source: &[u8], among_tests: bool) -> Result<SourceFile, Unreadable> {
    let text = std::str::from_utf8(source).map_err(|e| Unreadable { line: None, why: format!("is not UTF-8: {e}") })?;
    let mut parser = Parser::new();
    let parsed = parser.set_language(&tree_sitter_rust::LANGUAGE.into()).ok().and_then(|()| parser.parse(text, None));
    let Some(tree) = parsed else {
        return Err(Unreadable { line: None, why: "could not be parsed: tree-sitter gave no tree".into() });
    };
    let lines = syntax::Lines::of(source);
    if let Some(error) = first_error(tree.root_node()) {
        let file = Text::of_file(source);
        let what = if error.is_missing() {
            format!("expected `{}`", error.kind())
        } else {
            let token = syntax::tokens(error, file).into_iter().next().unwrap_or_default();
            format!("unexpected `{}`", first_line(token))
        };
        let line = lines.line(file.start(error));
        return Err(Unreadable { line: Some(line), why: format!("does not parse as Rust ({what})") });
    }
    let items = syntax::collect(&tree, Text::of_file(source), &lines, &mut parser);
    Ok(model(&items, text, among_tests, lines.count()))
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

/// The model of a file of `n` lines whose text is `text` and whose items are `items`.
fn model(items: &Items, text: &str, among_tests: bool, n: u32) -> SourceFile {
    let test_lines: Vec<Lines> = if among_tests {
        vec![Lines { first: 1, last: n }]
    } else {
        items.mods.iter().filter(|m| m.cfg_test).map(|m| Lines { first: m.attrs_line, last: m.end }).collect()
    };
    let in_tests = |line: u32| test_lines.iter().any(|l| l.holds(line));
    let tests: Vec<&Fn> = items
        .fns
        .iter()
        .filter(|f| f.attrs.iter().any(|a| a.path.rsplit("::").next() == Some("test")) && in_tests(f.line))
        .collect();
    let holding: Vec<&syntax::Mod> =
        items.mods.iter().filter(|m| tests.iter().any(|t| m.attrs_line <= t.line && t.line <= m.end)).collect();
    let mut weakenings = skips::skips(text, &tests, &holding, among_tests);
    weakenings.extend(swallowed_errors(items, &tests, &in_tests));
    let scored: Vec<&Fn> = items.fns.iter().filter(|f| f.flow.is_some()).collect();
    let calls = calls(&scored);
    SourceFile {
        functions: scored
            .iter()
            .zip(calls)
            .map(|(f, calls)| Function {
                name: qualified_name(f),
                line: f.line,
                lines: Lines { first: f.attrs_line, last: f.end },
                body: f.body.clone().unwrap_or_default(),
                exempt: in_tests(f.line),
                calls,
                flow: f.flow.as_ref().map(|(flow, _)| flow.clone()).unwrap_or_default(),
            })
            .collect(),
        tests: tests
            .iter()
            .map(|t| Test {
                name: qualified_name(t),
                line: t.line,
                lines: Lines { first: t.attrs_line, last: t.end },
                body: t.body.clone().unwrap_or_default(),
                assertions: t.asserts.clone(),
            })
            .collect(),
        test_groups: items
            .mods
            .iter()
            .map(|m| TestGroup { name: m.name.clone(), line: m.line, lines: Lines { first: m.attrs_line, last: m.end } })
            .collect(),
        test_lines,
        weakenings,
        bindings: items.bindings.clone(),
        constants: vec![],
    }
}

/// The swallowed errors among `items`, `tests` being its tests and `in_tests` saying whether a line is test code: a
/// `#[should_panic]` without `expected` on a test, which any panic passes; and a value thrown away in any fn among the
/// test code, a test or a helper, that holds an assertion macro the fn calls or a call of `catch_unwind`, so that the
/// panic or the `Err` that would fail the test is thrown away with it.
fn swallowed_errors(items: &Items, tests: &[&Fn], in_tests: &dyn std::ops::Fn(u32) -> bool) -> Vec<Weakening> {
    let mut out = vec![];
    for t in tests {
        for a in t.attrs.iter().filter(|a| a.path == "should_panic" && !a.text.contains("expected")) {
            out.push(Weakening {
                lines: Lines { first: a.line, last: a.line + u32::try_from(a.text.matches('\n').count()).unwrap_or(0) },
                kind: WeakeningKind::SwallowedError,
                code: model::without_whitespace(&a.text),
                message: format!(
                    "`{}` without `expected` added on `{}`: any panic passes it, a failed assertion's too.",
                    first_line(&a.text),
                    qualified_name(t)
                ),
            });
        }
    }
    for f in items.fns.iter().filter(|f| in_tests(f.line)) {
        for d in &f.discards {
            if d.text.contains("catch_unwind") || f.asserts.iter().any(|a| d.text.contains(&a.text)) {
                let written = d.written.replacen("{}", &d.text, 1);
                out.push(Weakening {
                    lines: d.lines,
                    kind: WeakeningKind::SwallowedError,
                    code: model::without_whitespace(&written),
                    message: format!(
                        "`{}` added in `{}`: it throws away the panic or the `Err` that would fail the test.",
                        first_line(&written),
                        qualified_name(f)
                    ),
                });
            }
        }
    }
    out
}

/// `f`'s name, qualified by the items that hold it and joined by `::`, as Rust writes such a path.
fn qualified_name(f: &Fn) -> String {
    let mut parts: Vec<String> = f
        .holders
        .iter()
        .map(|h| match h {
            Holder::Impl { ty, trait_: Some(t) } => format!("<{ty} as {t}>"),
            Holder::Impl { ty, trait_: None } => ty.clone(),
            Holder::Mod(n) | Holder::Trait(n) | Holder::Fn(n) | Holder::Const(n) | Holder::Static(n) => n.clone(),
        })
        .collect();
    parts.push(f.name.clone());
    parts.join("::")
}

/// A fn as a call names it: the inline modules it lies in, outermost first; the type of the impl it is in, None
/// outside one; the trait that impl implements, or that it is declared in, None for none, [`ANY_TRAIT`] standing for
/// any trait's impl of the type or its inherent impl; and its name. A type and a trait are each the identifier
/// [`path_name`] gives.
type FnKey = (Vec<String>, Option<String>, Option<String>, String);
const ANY_TRAIT: &str = "*";

/// For each of `functions`, those of a file the cognitive-complexity check scores, the indexes in `functions` of the
/// functions its calls may reach, the calls of the closures and fns its body holds included.
///
/// A call resolves by its path, within the modules the caller lies in: `f(...)` to the fn f outside any impl or
/// trait; `self.f(...)` and `Self::f(...)` to the fn f of the caller's impl or trait, failing that of an inherent impl
/// of its type, failing that of each impl of its type that has one; and `T::f(...)` to the fn f of an inherent impl of
/// the type T, failing that of each impl of T that has one, failing that of the trait T. Not to a trait's default fn
/// from an impl of it. A longer path, such as `crate::f(...)` or `super::f(...)`, `m::f(...)` for a module m, a local
/// closure of a fn's name, a method on anything but self and a call through a trait object resolve to none.
fn calls(functions: &[&Fn]) -> Vec<Vec<usize>> {
    let scopes: Vec<_> = functions.iter().map(|f| scope(f)).collect();
    let mut by_key: HashMap<FnKey, Vec<usize>> = HashMap::new();
    for (i, (f, (mods, ty, trait_))) in functions.iter().zip(&scopes).enumerate() {
        by_key.entry((mods.clone(), ty.clone(), trait_.clone(), f.name.clone())).or_default().push(i);
        if ty.is_some() {
            by_key.entry((mods.clone(), ty.clone(), Some(ANY_TRAIT.into()), f.name.clone())).or_default().push(i);
        }
    }
    let mut out = vec![];
    for (f, (mods, ty, trait_)) in functions.iter().zip(&scopes) {
        let mut reached = BTreeSet::new();
        for path in f.flow.as_ref().map(|(_, calls)| calls.as_slice()).unwrap_or_default() {
            let callees = callees(path, mods, ty.as_ref(), trait_.as_ref());
            if let Some(found) = callees.iter().find_map(|key| by_key.get(key)) {
                reached.extend(found);
            }
        }
        out.push(reached.into_iter().collect());
    }
    out
}

/// The modules, type and trait of `f`, as [`FnKey`] has them: those of its innermost impl or trait.
fn scope(f: &Fn) -> (Vec<String>, Option<String>, Option<String>) {
    let mods = f.holders.iter().filter_map(|h| if let Holder::Mod(n) = h { Some(n.clone()) } else { None }).collect();
    match f.holders.iter().rev().find(|h| matches!(h, Holder::Impl { .. } | Holder::Trait(_))) {
        Some(Holder::Trait(name)) => (mods, None, Some(name.clone())),
        Some(Holder::Impl { ty, trait_ }) => (mods, path_name(ty), trait_.as_deref().and_then(path_name)),
        _ => (mods, None, None),
    }
}

/// The identifier that names the type or trait `text`, as written: its last identifier before any generic arguments,
/// `Formatter` for `&'a mut fmt::Formatter<'_>`, which is what a call `Formatter::f(...)` names it by; None for none,
/// as for `()`.
fn path_name(text: &str) -> Option<String> {
    let head = text.split('<').next().unwrap_or_default();
    let mut words = head.split(|c: char| !(c.is_alphanumeric() || c == '_')).filter(|w| !w.is_empty());
    words.next_back().map(String::from)
}

/// The fns, as [`FnKey`] has them, that a call of `path` from a fn of the modules `mods`, type `ty` and trait `trait_`
/// may make, as [`calls`] resolves calls, in the order it tries them; none for a call it resolves to none.
fn callees(path: &[String], mods: &[String], ty: Option<&String>, trait_: Option<&String>) -> Vec<FnKey> {
    let key = |ty: Option<&String>, trait_: Option<&str>, name: &String| {
        (mods.to_vec(), ty.cloned(), trait_.map(String::from), name.clone())
    };
    match path {
        [name] => vec![key(None, None, name)],
        [own, name] if own == "self" || own == "Self" => match ty {
            // A fn outside any impl or trait has no self, and a trait's own fn, whose type is None, no inherent impl.
            None => trait_.map(|t| vec![key(None, Some(t), name)]).unwrap_or_default(),
            Some(ty) => {
                vec![key(Some(ty), trait_.map(String::as_str), name), key(Some(ty), None, name), key(Some(ty), Some(ANY_TRAIT), name)]
            }
        },
        [named, name] => vec![key(Some(named), None, name), key(Some(named), Some(ANY_TRAIT), name), key(None, Some(named), name)],
        _ => vec![],
    }
}
