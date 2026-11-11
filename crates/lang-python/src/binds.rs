//! What one node of a Python file's tree binds: the one reading of Python's binding forms that the vague-names
//! bindings, the names a test binds, a scope's bindings and the resolution of calls all go by.

use tree_sitter::{Node, Point};

use crate::syntax::{children, fields, named, of_kind, unparenthesized, Text};

/// How a name is bound, which decides who counts the binding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum How {
    /// By a def's or class's own name, an assignment, a loop, a with's or except's `as`, an import, a match capture
    /// or a type alias: in the scope the node is read in.
    Name,
    /// As a parameter of the def or lambda the node is: in that function's own scope.
    Parameter,
    /// As a PEP 695 type parameter of the def, class or type alias the node is.
    TypeParameter,
    /// By assigning to an attribute of `self` or `cls`: no name of any scope.
    SelfAttribute,
    /// By `:=`, which binds in the nearest scope around that is no comprehension's (PEP 572).
    Walrus,
}

/// A name a node binds.
#[derive(Clone, Copy, Debug)]
pub struct Bound<'t> {
    pub name: &'t str,
    /// Where Python's ast places the binding: the `def`, `class` or `except` keyword, a parameter's or target's own
    /// name, an import's first name, a pattern's start.
    pub at: Point,
    pub how: How,
}

/// The kinds of node that open a scope of their own inside a module or a function: of what [`binds`] gives for one,
/// only its [`How::Name`] is bound in the scope around it.
pub const SCOPES: [&str; 7] = [
    "function_definition",
    "class_definition",
    "lambda",
    "list_comprehension",
    "set_comprehension",
    "dictionary_comprehension",
    "generator_expression",
];

/// The names `node` itself binds, in the order they are written; none for a node that binds nothing, and nothing of
/// what the nodes it holds bind, but for a `case` clause, which stands for its whole pattern. `global`, `nonlocal`
/// and `del` bind nothing new: [`declared`] and [`deleted`] read them.
pub fn binds<'t>(node: Node, text: Text<'t>) -> Vec<Bound<'t>> {
    let name_at = |name: Node, at: Node, how: How| Bound { name: text.of(name), at: at.start_position(), how };
    let mut out = vec![];
    match node.kind() {
        "function_definition" | "class_definition" | "lambda" => {
            out.extend(node.child_by_field_name("name").map(|name| name_at(name, node, How::Name)));
            let parameters = node.child_by_field_name("parameters").map(parameters).unwrap_or_default();
            out.extend(parameters.into_iter().map(|p| name_at(p, p, How::Parameter)));
            out.extend(type_parameters(node, text));
        }
        "assignment" | "augmented_assignment" | "for_statement" | "for_in_clause" => {
            out.extend(fields(node, "left").into_iter().flat_map(|left| targets(left, text)));
        }
        "with_item" => {
            // `with (open(p) as f):` holds its one item in parentheses, and `with (open(p) as f,):` in a tuple.
            let items = match node.child_by_field_name("value").map(unparenthesized) {
                Some(tuple) if tuple.kind() == "tuple" => named(tuple),
                value => value.into_iter().collect(),
            };
            let aliases = items.into_iter().filter(|v| v.kind() == "as_pattern");
            let aliases = aliases.filter_map(|v| v.child_by_field_name("alias"));
            out.extend(aliases.flat_map(|alias| targets(alias, text)));
        }
        "except_clause" | "except_group_clause" => {
            let alias = of_kind(node, "as_pattern").first().and_then(|v| v.child_by_field_name("alias"));
            let name = alias.and_then(|a| named(a).into_iter().find(|n| n.kind() == "identifier"));
            out.extend(name.map(|name| name_at(name, node, How::Name)));
        }
        "named_expression" => {
            let name = node.child_by_field_name("name").filter(|n| n.kind() == "identifier");
            out.extend(name.map(|name| name_at(name, name, How::Walrus)));
        }
        "import_statement" | "import_from_statement" | "future_import_statement" => out.extend(imported(node, text)),
        "case_clause" => {
            out.extend(of_kind(node, "case_pattern").into_iter().flat_map(|pattern| captures(pattern, text)));
        }
        "type_alias_statement" => out.extend(type_alias(node, text)),
        _ => {}
    }
    out
}

/// The names the import statement `statement` binds, each placed at what it imports: `import a.b` binds a, and
/// `import a.b as c` and `from m import a as c` bind c.
fn imported<'t>(statement: Node, text: Text<'t>) -> Vec<Bound<'t>> {
    let mut out = vec![];
    for imported in fields(statement, "name") {
        let name = match imported.kind() {
            "aliased_import" => imported.child_by_field_name("alias"),
            _ => named(imported).into_iter().next(),
        };
        if let Some(name) = name.filter(|n| n.kind() == "identifier") {
            out.push(Bound { name: text.of(name), at: imported.start_position(), how: How::Name });
        }
    }
    out
}

/// The names the `type` statement `statement` binds: the alias, and the type parameters of `type X[T] = ...`.
fn type_alias<'t>(statement: Node, text: Text<'t>) -> Vec<Bound<'t>> {
    // The alias, with its type parameters, is the statement's first `type` child.
    let Some(left) = named(statement).into_iter().next().and_then(|t| named(t).into_iter().next()) else {
        return vec![];
    };
    let name = match left.kind() {
        "identifier" => Some(left),
        _ => named(left).into_iter().find(|n| n.kind() == "identifier"),
    };
    let mut out: Vec<Bound> =
        name.map(|n| Bound { name: text.of(n), at: n.start_position(), how: How::Name }).into_iter().collect();
    out.extend(of_kind(left, "type_parameter").into_iter().flat_map(|list| listed_type_parameters(list, text)));
    out
}

/// The names the `global` or `nonlocal` statement `node` declares; none for any other node.
pub fn declared<'t>(node: Node, text: Text<'t>) -> Vec<&'t str> {
    named(node).into_iter().filter(|n| n.kind() == "identifier").map(|n| text.of(n)).collect()
}

/// The names the `del` statement `node` unbinds, which Python counts as bound in their scope; none for any other
/// node, nor for an attribute or a subscript it deletes.
pub fn deleted<'t>(node: Node, text: Text<'t>) -> Vec<&'t str> {
    if node.kind() != "delete_statement" {
        return vec![];
    }
    let stored = named(node).into_iter().flat_map(|target| targets(target, text));
    stored.filter(|b| b.how == How::Name).map(|b| b.name).collect()
}

/// What assigning to `target` binds: `target` when it is a name, the names in it when it is a tuple, a list, a
/// starred or parenthesized target, and the attribute when it is one of `self` or `cls`. A subscript and any other
/// attribute bind nothing.
fn targets<'t>(target: Node, text: Text<'t>) -> Vec<Bound<'t>> {
    let mut out = vec![];
    let mut todo = vec![target];
    while let Some(node) = todo.pop() {
        match node.kind() {
            "identifier" => out.push(Bound { name: text.of(node), at: node.start_position(), how: How::Name }),
            "pattern_list" | "tuple_pattern" | "list_pattern" | "list_splat_pattern" | "list_splat" | "tuple"
            | "list" | "parenthesized_expression" | "as_pattern_target" | "expression_list" => {
                todo.extend(named(node).into_iter().rev());
            }
            "attribute" => {
                let object = node.child_by_field_name("object").filter(|o| o.kind() == "identifier");
                let own = object.is_some_and(|o| matches!(text.of(o), "self" | "cls"));
                if let Some(attribute) = node.child_by_field_name("attribute").filter(|_| own) {
                    out.push(Bound { name: text.of(attribute), at: node.start_position(), how: How::SelfAttribute });
                }
            }
            _ => {}
        }
    }
    out
}

/// The name of each parameter of `parameters`, a def's `parameters` or a lambda's `lambda_parameters`, in order.
pub fn parameters(parameters: Node) -> Vec<Node> {
    let mut out = vec![];
    for parameter in named(parameters) {
        let name = match parameter.kind() {
            "identifier" => Some(parameter),
            "default_parameter" | "typed_default_parameter" => parameter.child_by_field_name("name"),
            // `x: int`, `*args: int`, `**kwargs: int`: the name, or the starred name, comes first.
            "typed_parameter" => named(parameter).into_iter().next(),
            "list_splat_pattern" | "dictionary_splat_pattern" => Some(parameter),
            _ => None,
        };
        let name = name.map(|n| match n.kind() {
            "list_splat_pattern" | "dictionary_splat_pattern" => named(n).into_iter().next().unwrap_or(n),
            _ => n,
        });
        out.extend(name.filter(|n| n.kind() == "identifier"));
    }
    out
}

/// The PEP 695 type parameters of `definition`, a def or a class: none for one without, or for any other node.
pub fn type_parameters<'t>(definition: Node, text: Text<'t>) -> Vec<Bound<'t>> {
    // The field, not the kind: tree-sitter gives a subscript in an annotation the kind `type_parameter` too.
    definition.child_by_field_name("type_parameters").map(|list| listed_type_parameters(list, text)).unwrap_or_default()
}

/// The type parameters `list`, a bracketed list of them, declares.
fn listed_type_parameters<'t>(list: Node, text: Text<'t>) -> Vec<Bound<'t>> {
    let mut out = vec![];
    for parameter in of_kind(list, "type") {
        // `T`, `*Ts`, `**P` or `T: bound`.
        let mut name = named(parameter).into_iter().next();
        while let Some(inner) = name.filter(|n| n.kind() != "identifier") {
            name = named(inner).into_iter().next();
        }
        if let Some(name) = name {
            out.push(Bound { name: text.of(name), at: parameter.start_position(), how: How::TypeParameter });
        }
    }
    out
}

/// The bounds of `definition`'s type parameters, as in `def f[T: int]()`, which Python reads in the type parameters'
/// own scope.
pub fn type_parameter_bounds(definition: Node) -> Vec<Node> {
    let Some(list) = definition.child_by_field_name("type_parameters") else { return vec![] };
    let constrained = of_kind(list, "type").into_iter().flat_map(|t| of_kind(t, "constrained_type"));
    constrained.flat_map(|c| named(c).into_iter().skip(1)).collect()
}

/// The names `pattern`, a `case` clause's pattern, captures, in the order written: a bare name, the name after `as`,
/// `*rest` in a sequence and `**rest` in a mapping, the last placed at the mapping's start as Python's ast places it.
fn captures<'t>(pattern: Node, text: Text<'t>) -> Vec<Bound<'t>> {
    let mut out = vec![];
    let mut todo = vec![pattern];
    while let Some(node) = todo.pop() {
        let name_at = |name: Node, at: Node| Bound { name: text.of(name), at: at.start_position(), how: How::Name };
        match node.kind() {
            "dotted_name" => {
                // A dotted name of one part captures; of more, it is a value to compare with, as a class's name is.
                let is_class = node.parent().is_some_and(|p| p.kind() == "class_pattern")
                    && node.prev_named_sibling().is_none();
                if let ([name], false) = (named(node).as_slice(), is_class) {
                    out.push(name_at(*name, *name));
                }
            }
            "as_pattern" => {
                let parts = children(node);
                let name = parts.last().copied().filter(|n| n.kind() == "identifier");
                out.extend(name.map(|name| name_at(name, node)));
                todo.extend(parts.into_iter().rev().filter(|p| Some(*p) != name));
            }
            "splat_pattern" => {
                let name = named(node).into_iter().find(|n| n.kind() == "identifier");
                out.extend(name.map(|name| name_at(name, mapping_of(node).unwrap_or(node))));
            }
            "keyword_pattern" => todo.extend(named(node).into_iter().skip(1).rev()),
            _ => todo.extend(named(node).into_iter().rev()),
        }
    }
    out
}

/// The mapping pattern that `splat`, a `splat_pattern`, is the `**rest` of; None for a sequence's `*rest`.
fn mapping_of(splat: Node) -> Option<Node> {
    let mut holder = splat.parent();
    while let Some(wrapper) = holder.filter(|h| h.kind() == "case_pattern") {
        holder = wrapper.parent();
    }
    holder.filter(|h| h.kind() == "dict_pattern")
}
