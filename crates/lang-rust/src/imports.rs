//! The imports between a tree's Rust files: each path of a `use`, or of code, that starts at `crate`, `self` or
//! `super`, resolved through the tree's module tree to the file that holds the module it names.

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

use model::{Import, ImportGraph};
use tree_sitter::{Node, Parser};

use crate::syntax::{holds_only_under_test, named, preceding_attributes, unraw, Text};

/// The imports between `files`, each Rust file of one tree as (path from the tree's top level, its bytes): every file
/// of `files` keyed, with the imports it makes of the others.
///
/// The files make modules as rustc reads them: a crate root is a file no `mod` declaration of the tree reaches (a
/// `lib.rs`, a `main.rs`, a binary, a test or an example), and `mod x;` in a file is `x.rs` or `x/mod.rs` beside it
/// when the file is a crate root or a `mod.rs`, below the directory of the file's own name otherwise, below the inline
/// modules it sits in, or the file a `#[path = "..."]` on a declaration outside inline modules names. A file reached
/// from several crate roots, as `tests/common/mod.rs`, is a module of each.
///
/// An import is a path, in a `use` (each of a `use` tree's leaves) or in code, that starts at `crate`, `self` or
/// `super`, of the file of the longest of its prefixes that is a module, an inline module being its file's; it lies at
/// the line of its last segment. Left out are: a path that starts anywhere else, as at another crate; an import of
/// the file's own modules; one between a module and a module it holds, at any depth, as `super::` from a child, so
/// that a crate root takes part in no cycle; and every path in test code, an item marked `#[cfg(test)]` and every file
/// only such items declare. A macro's arguments are not read. No import is [`Import::deferred`].
///
/// A file that is not UTF-8, or does not parse as Rust, makes no imports, and declares no modules.
pub fn imports(files: &[(OsString, Vec<u8>)]) -> ImportGraph {
    let parsed: Vec<Option<Parsed>> = files.iter().map(|(_, source)| parse(source)).collect();
    let paths: Vec<PathBuf> = files.iter().map(|(path, _)| normal(Path::new(path))).collect();
    let tree = ModuleTree::of(&paths, &parsed);
    let mut out = ImportGraph::new();
    for (i, (path, _)) in files.iter().enumerate() {
        out.insert(path.clone(), tree.imports(i, parsed[i].as_ref(), files));
    }
    out
}

/// What [`imports`] reads of one file.
#[derive(Default)]
struct Parsed {
    /// Its `mod x;` declarations, those of inline modules too.
    declarations: Vec<Declaration>,
    /// The path within the file, outermost first, of each of its inline modules.
    inline: Vec<Vec<String>>,
    /// Its paths outside test code that start at `crate`, `self` or `super`.
    paths: Vec<UsedPath>,
}

/// A `mod x;` declaration, of a module whose body is another file.
struct Declaration {
    /// The inline modules it sits in, outermost first.
    scope: Vec<String>,
    name: String,
    /// Its `#[path = "..."]`'s file, relative to the declaring file's directory; None without one, or inside an inline
    /// module, where rustc reads it from elsewhere.
    path: Option<String>,
    /// Whether it, or an inline module it sits in, is marked `#[cfg(test)]`.
    test: bool,
}

/// A path as an import names it.
struct UsedPath {
    /// The inline modules it is written in, outermost first.
    scope: Vec<String>,
    /// Its segments, `crate`, `self` or `super` first.
    segments: Vec<String>,
    line: u32,
}

/// The file `source`, read as [`imports`] reads it; None where it is not UTF-8 or does not parse.
fn parse(source: &[u8]) -> Option<Parsed> {
    std::str::from_utf8(source).ok()?;
    let mut parser = Parser::new();
    parser.set_language(&tree_sitter_rust::LANGUAGE.into()).ok()?;
    let tree = parser.parse(source, None)?;
    let root = tree.root_node();
    if root.has_error() {
        return None;
    }
    let text = Text::of_file(source);
    let test = inner_test(root, text);
    let mut out = Parsed::default();
    walk(root, text, &mut vec![], test, &mut out);
    Some(out)
}

/// Adds to `out` what `node` and below declare and import, `scope` being the inline modules it sits in and `test`
/// whether it is test code.
fn walk(node: Node, text: Text, scope: &mut Vec<String>, test: bool, out: &mut Parsed) {
    match node.kind() {
        "mod_item" => {
            let name = node.child_by_field_name("name").map(|n| unraw(text.of(n)).to_string()).unwrap_or_default();
            let Some(body) = node.child_by_field_name("body") else {
                let path = scope.is_empty().then(|| path_attribute(node, text)).flatten();
                out.declarations.push(Declaration { scope: scope.clone(), name, path, test });
                return;
            };
            let test = test || inner_test(body, text);
            scope.push(name);
            out.inline.push(scope.clone());
            walk(body, text, scope, test, out);
            scope.pop();
        }
        "use_declaration" if !test => {
            if let Some(argument) = node.child_by_field_name("argument") {
                use_tree(argument, text, &[], &mut |segments, line| {
                    out.paths.push(UsedPath { scope: scope.clone(), segments, line });
                });
            }
        }
        "scoped_identifier" | "scoped_type_identifier" if !test => match segments(node, text) {
            Some(segments) => {
                if matches!(segments[0].as_str(), "crate" | "self" | "super") {
                    out.paths.push(UsedPath { scope: scope.clone(), segments, line: line(node) });
                }
            }
            None => walk_children(node, text, scope, test, out),
        },
        "use_declaration" | "scoped_identifier" | "scoped_type_identifier" | "attribute_item"
        | "inner_attribute_item" | "visibility_modifier" => {}
        _ => walk_children(node, text, scope, test, out),
    }
}

/// [`walk`]s each of `node`'s named children in order, as test code where `test` or where an attribute right before
/// it holds only under `cfg(test)`: tree-sitter makes an item's outer attributes its siblings.
fn walk_children(node: Node, text: Text, scope: &mut Vec<String>, test: bool, out: &mut Parsed) {
    let mut marked = false;
    for child in named(node) {
        match child.kind() {
            "attribute_item" => marked |= holds_only_under_test(child, text),
            "line_comment" | "block_comment" => {}
            _ => {
                walk(child, text, scope, test || marked, out);
                marked = false;
            }
        }
    }
}

/// Whether `body`, a file's or an inline module's, starts with `#![cfg(test)]`, making the whole of it test code.
fn inner_test(body: Node, text: Text) -> bool {
    named(body).into_iter().any(|n| n.kind() == "inner_attribute_item" && holds_only_under_test(n, text))
}

/// Calls `leaf` with each path the `use` tree `node` brings in, `prefix` before it, and the line of the path's last
/// segment. A `self` in a list is the path the list follows; a glob is the path it follows.
fn use_tree(node: Node, text: Text, prefix: &[String], leaf: &mut dyn FnMut(Vec<String>, u32)) {
    let joined = |path: Option<Node>| -> Option<Vec<String>> {
        let mut out = prefix.to_vec();
        out.extend(segments(path?, text)?);
        Some(out)
    };
    match node.kind() {
        "scoped_use_list" => {
            let Some(path) = joined(node.child_by_field_name("path")) else { return };
            if let Some(list) = node.child_by_field_name("list") {
                use_tree(list, text, &path, leaf);
            }
        }
        "use_list" => named(node).into_iter().for_each(|child| use_tree(child, text, prefix, leaf)),
        "use_as_clause" => {
            if let Some(path) = node.child_by_field_name("path") {
                use_tree(path, text, prefix, leaf);
            }
        }
        "use_wildcard" => match named(node).into_iter().find(|c| !c.is_extra()) {
            Some(path) => use_tree(path, text, prefix, leaf),
            None if !prefix.is_empty() => leaf(prefix.to_vec(), line(node)),
            None => {}
        },
        _ => {
            let Some(mut path) = joined(Some(node)) else { return };
            if path.len() > 1 && path.last().is_some_and(|s| s == "self") {
                path.pop();
            }
            leaf(path, line(node));
        }
    }
}

/// The segments of the plain path `node`, unraw, outermost first; None for one with a generic or a bracketed type in
/// it, or a macro's metavariable.
fn segments(node: Node, text: Text) -> Option<Vec<String>> {
    match node.kind() {
        "scoped_identifier" | "scoped_type_identifier" => {
            let mut out = segments(node.child_by_field_name("path")?, text)?;
            out.push(unraw(text.of(node.child_by_field_name("name")?)).to_string());
            Some(out)
        }
        "identifier" | "type_identifier" | "crate" | "self" | "super" => Some(vec![unraw(text.of(node)).to_string()]),
        _ => None,
    }
}

/// The line, from 1, `node` ends on, where the last segment of a path is.
fn line(node: Node) -> u32 {
    u32::try_from(node.end_position().row + 1).unwrap_or(u32::MAX)
}

/// The value of `#[path = "..."]` on the declaration `node`, without its quotes.
fn path_attribute(node: Node, text: Text) -> Option<String> {
    preceding_attributes(node).into_iter().find_map(|item| {
        let attribute = named(item).into_iter().find(|c| c.kind() == "attribute")?;
        let is_path = attribute.named_child(0).is_some_and(|p| text.of(p) == "path");
        let value = attribute.child_by_field_name("value").filter(|v| is_path && v.kind() == "string_literal")?;
        Some(text.of(value).trim_matches('"').to_string())
    })
}

/// `path` with its `.` components dropped and each `..` taking the one before it away.
fn normal(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// A module of the tree: the crate root it is reached from, by its index among the roots, and its path from there,
/// outermost first.
type Module = (usize, Vec<String>);

/// The modules a tree's files make.
struct ModuleTree {
    /// Each file's modules, by the file's index, with whether only test code declares it there; none for a file no
    /// crate root reaches, which only a `mod` declaration that loops back can leave.
    of_file: Vec<Vec<(Module, bool)>>,
    /// The file of each module, inline ones too.
    files: HashMap<Module, usize>,
}

impl ModuleTree {
    /// The modules of `paths`, the files' paths normalized, which `parsed` holds the declarations of, by index.
    fn of(paths: &[PathBuf], parsed: &[Option<Parsed>]) -> ModuleTree {
        let index: HashMap<&Path, usize> = paths.iter().enumerate().map(|(i, p)| (p.as_path(), i)).collect();
        let declared = |i: usize, root: bool| -> Vec<(usize, &Declaration)> {
            let Some(parsed) = &parsed[i] else { return vec![] };
            parsed
                .declarations
                .iter()
                .filter_map(|d| Some((declared_file(&paths[i], root, d, &index)?, d)))
                .collect()
        };
        // A crate root is a file no declaration reaches, whether its file is read as a crate root or not.
        let reached: HashSet<usize> = (0..paths.len())
            .flat_map(|i| [true, false].map(|root| declared(i, root)))
            .flatten()
            .map(|(f, _)| f)
            .collect();
        let mut roots: VecDeque<usize> = (0..paths.len()).filter(|i| !reached.contains(i)).collect();
        let mut tree = ModuleTree { of_file: vec![vec![]; paths.len()], files: HashMap::new() };
        let mut root = 0;
        while let Some(file) = roots.pop_front() {
            let mut queue = VecDeque::from([(file, vec![], false)]);
            let mut seen = HashSet::new();
            while let Some((file, path, test)) = queue.pop_front() {
                if !seen.insert(file) {
                    continue;
                }
                for (child, d) in declared(file, path.is_empty()) {
                    let mut child_path = path.clone();
                    child_path.extend(d.scope.iter().cloned());
                    child_path.push(d.name.clone());
                    queue.push_back((child, child_path, test || d.test));
                }
                for inline in parsed[file].iter().flat_map(|p| &p.inline) {
                    let mut inline_path = path.clone();
                    inline_path.extend(inline.iter().cloned());
                    tree.files.insert((root, inline_path), file);
                }
                tree.files.insert((root, path.clone()), file);
                tree.of_file[file].push(((root, path), test));
            }
            root += 1;
            // A file only declarations that loop back reach is a crate root of its own.
            if roots.is_empty() {
                roots.extend((0..paths.len()).find(|&i| tree.of_file[i].is_empty()));
            }
        }
        tree
    }

    /// The imports of `files[i]`, whose parse is `parsed`, as [`imports`] has them.
    fn imports(&self, i: usize, parsed: Option<&Parsed>, files: &[(OsString, Vec<u8>)]) -> Vec<Import> {
        let Some(parsed) = parsed else { return vec![] };
        let mut found = BTreeSet::new();
        let mut out = vec![];
        for ((root, own), _) in self.of_file[i].iter().filter(|(_, test)| !test) {
            for used in &parsed.paths {
                let Some(target) = self.resolve(*root, own, used) else { continue };
                if found.insert((target, used.line)) {
                    out.push(Import { module: files[target].0.clone(), line: used.line, deferred: false });
                }
            }
        }
        out
    }

    /// The file `used`, a path of the file of module `own` of crate root `root`, imports; None for its own, one it
    /// holds or that holds it, and for a path that names no module of the root.
    fn resolve(&self, root: usize, own: &[String], used: &UsedPath) -> Option<usize> {
        let mut path: Vec<String> = own.iter().chain(&used.scope).cloned().collect();
        let mut rest = used.segments.as_slice();
        if rest.first().is_some_and(|s| s == "crate") {
            path.clear();
            rest = &rest[1..];
        }
        while let Some((first, after)) = rest.split_first() {
            match first.as_str() {
                "self" => {}
                "super" => {
                    path.pop()?;
                }
                _ => break,
            }
            rest = after;
        }
        let at = path.len();
        path.extend(rest.iter().cloned());
        let file = (at..=path.len()).rev().find_map(|n| self.files.get(&(root, path[..n].to_vec())).copied())?;
        // The target file's own module of the root, which an inline module the path names lies in.
        let ((_, target), _) = self.of_file[file].iter().find(|((r, _), _)| *r == root)?;
        let nested = own.starts_with(target) || target.starts_with(own);
        (!nested).then_some(file)
    }
}

/// The index of the file the declaration `d` in the file `path` names, read as in a crate root's file when `root`;
/// None when `index`, the tree's files by normalized path, has none.
fn declared_file(path: &Path, root: bool, d: &Declaration, index: &HashMap<&Path, usize>) -> Option<usize> {
    let parent = path.parent().unwrap_or(Path::new(""));
    if let Some(attribute) = &d.path {
        return index.get(normal(&parent.join(attribute)).as_path()).copied();
    }
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or_default();
    let mut dir = if root || matches!(stem, "lib" | "main" | "mod") { parent.to_path_buf() } else { parent.join(stem) };
    dir.extend(&d.scope);
    [dir.join(format!("{}.rs", d.name)), dir.join(&d.name).join("mod.rs")]
        .iter()
        .find_map(|candidate| index.get(candidate.as_path()).copied())
}
