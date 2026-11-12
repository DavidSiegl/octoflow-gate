//! The imports between a tree's Python files: each `import` and `from … import` resolved to the file of the tree that
//! Python would load for it.

use std::collections::HashSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use model::{Import, ImportGraph};
use tree_sitter::Node;

use crate::syntax::{fields, line, named, Text};

/// The directories, from the tree's top level, that an absolute import is looked up from, in order: the top, then
/// `src`, the two layouts a project's own packages take.
const ROOTS: [&str; 2] = ["", "src"];

/// The imports between `files`, each Python file of one tree as (path from the tree's top level, its bytes): every
/// file of `files` keyed, with the imports it makes of the others.
///
/// An absolute import names a module under one of [`ROOTS`], `a.b` being `a/b.py` or `a/b/__init__.py`, the first
/// root that holds one; `import a.b.c` is of the deepest of `a.b.c`, `a.b` and `a` there is a file of. A relative
/// one is read from the importing file's directory, one level up for each dot after the first. `from m import n` is of
/// the submodule `m.n` where there is a file of it, else of `m`. An import that names no file of the tree is a third
/// party's or the standard library's, and none.
///
/// An import inside a function is [`Import::deferred`]; one in the body of `if TYPE_CHECKING:` (or
/// `typing.TYPE_CHECKING`) is none, as it never runs. A file that cannot be decoded, or does not parse as Python, makes
/// no imports.
pub fn imports(files: &[(OsString, Vec<u8>)]) -> ImportGraph {
    let present: HashSet<&Path> = files.iter().map(|(path, _)| Path::new(path)).collect();
    let mut out = ImportGraph::new();
    for (path, source) in files {
        let found = crate::decode::decode(source)
            .ok()
            .and_then(|text| crate::parse(&text).ok().map(|tree| (tree, text)))
            .map(|(tree, text)| {
                let mut found = vec![];
                let resolver = Resolver { present: &present, file: Path::new(path) };
                collect(tree.root_node(), Text(&text), &resolver, false, &mut found);
                found
            });
        out.insert(path.clone(), found.unwrap_or_default());
    }
    out
}

/// What an import is resolved against: the tree's Python files, and the importing one.
struct Resolver<'a> {
    present: &'a HashSet<&'a Path>,
    file: &'a Path,
}

impl Resolver<'_> {
    /// The file of the module `segments` names below `dir`: `dir/a/b.py` or `dir/a/b/__init__.py` for `a.b`, and the
    /// package `dir/__init__.py` for none. None where the tree holds neither, or for the importing file itself.
    fn module(&self, dir: &Path, segments: &[&str]) -> Option<PathBuf> {
        let mut package = dir.to_path_buf();
        package.extend(segments);
        let as_file = (!segments.is_empty()).then(|| package.with_extension("py"));
        as_file
            .into_iter()
            .chain([package.join("__init__.py")])
            .find(|candidate| self.present.contains(candidate.as_path()) && candidate != self.file)
    }

    /// The file of the deepest of `dotted` (`a.b.c`) and its prefixes there is one of, looked up from each of
    /// [`ROOTS`] in turn.
    fn absolute(&self, dotted: &str) -> Option<PathBuf> {
        let segments: Vec<&str> = dotted.split('.').collect();
        let deepest = |root: &&str| {
            (1..=segments.len()).rev().find_map(|n| self.module(Path::new(root), &segments[..n]))
        };
        ROOTS.iter().find_map(deepest)
    }

    /// The directory a relative import of `dots` dots (`.` is one) starts from: the importing file's own, one level up
    /// for each dot after the first. None where it would rise above the tree's top.
    fn relative_dir(&self, dots: usize) -> Option<PathBuf> {
        let mut dir = self.file.parent()?.to_path_buf();
        for _ in 1..dots {
            if dir.as_os_str().is_empty() {
                return None;
            }
            dir = dir.parent()?.to_path_buf();
        }
        Some(dir)
    }
}

/// Appends to `out` the imports in `node` and below, as [`imports`] says, `deferred` when `node` lies in a function.
fn collect(node: Node, text: Text, resolver: &Resolver, deferred: bool, out: &mut Vec<Import>) {
    match node.kind() {
        "import_statement" => {
            for name in fields(node, "name") {
                let dotted = name.child_by_field_name("name").unwrap_or(name);
                if let Some(module) = resolver.absolute(text.of(dotted)) {
                    out.push(Import { module: module.into(), line: line(name), deferred });
                }
            }
            return;
        }
        "import_from_statement" => {
            from_import(node, text, resolver, deferred, out);
            return;
        }
        "function_definition" | "lambda" => {
            for child in named(node) {
                collect(child, text, resolver, true, out);
            }
            return;
        }
        "if_statement" if type_checking(node, text) => {
            // Only the body never runs; an `else:` or `elif` beside it does.
            for child in fields(node, "alternative") {
                collect(child, text, resolver, deferred, out);
            }
            return;
        }
        _ => {}
    }
    for child in named(node) {
        collect(child, text, resolver, deferred, out);
    }
}

/// Appends to `out` the imports of `from … import …` statement `node`.
fn from_import(node: Node, text: Text, resolver: &Resolver, deferred: bool, out: &mut Vec<Import>) {
    let Some(module) = node.child_by_field_name("module_name") else { return };
    // Where the module is looked up from, and its own segments: a relative import's after its dots.
    let (dirs, dotted): (Vec<PathBuf>, &str) = if module.kind() == "relative_import" {
        let written = text.of(module);
        let dots = written.len() - written.trim_start_matches('.').len();
        (resolver.relative_dir(dots).into_iter().collect(), written[dots..].trim())
    } else {
        (ROOTS.iter().map(PathBuf::from).collect(), text.of(module))
    };
    let segments: Vec<&str> = dotted.split('.').filter(|s| !s.is_empty()).collect();
    let names = fields(node, "name");
    let named_ones: Vec<(Option<&str>, u32)> = if names.is_empty() {
        vec![(None, line(node))]
    } else {
        names
            .iter()
            .map(|n| (Some(text.of(n.child_by_field_name("name").unwrap_or(*n))), line(*n)))
            .collect()
    };
    for (name, at) in named_ones {
        let submodule = |dir: &PathBuf| {
            let name = name?;
            let mut longer = segments.clone();
            longer.push(name);
            resolver.module(dir, &longer)
        };
        let found = dirs.iter().find_map(|dir| submodule(dir).or_else(|| resolver.module(dir, &segments)));
        if let Some(module) = found {
            out.push(Import { module: module.into(), line: at, deferred });
        }
    }
}

/// Whether `if_statement` `node` tests `TYPE_CHECKING` or `typing.TYPE_CHECKING`, alone.
fn type_checking(node: Node, text: Text) -> bool {
    node.child_by_field_name("condition")
        .is_some_and(|c| matches!(text.of(c), "TYPE_CHECKING" | "typing.TYPE_CHECKING"))
}
