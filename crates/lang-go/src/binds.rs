//! The names a Go file binds, for the vague-names check.

use model::Binding;
use tree_sitter::Node;

use crate::syntax::{children, fields, line, named, Text};

/// Every name `root`, a file's tree, binds, in the order they are written: the name of a function, a method, a type
/// or alias and an interface's method; each name of a parameter, a receiver and a named result, of a `func` literal's
/// too; each name a `var` or `const` declares; each name a `:=` binds, in a statement, an `if`'s, `for`'s or
/// `switch`'s initializer, a `range` clause, a type switch or a `select` case; and an import's own name. `_` binds
/// nothing. None is [`Binding::fixed`]: a method may implement an interface, which fixes its name, but which
/// interfaces a type implements cannot be told from its file, as a Python override without `@override` cannot.
pub fn bindings(root: Node, text: Text) -> Vec<Binding> {
    let mut out = vec![];
    // Nodes still to read, the next one last.
    let mut pending = vec![root];
    while let Some(node) = pending.pop() {
        let bound: Vec<Node> = match node.kind() {
            "function_declaration" | "method_declaration" | "type_spec" | "type_alias" | "method_elem" => {
                node.child_by_field_name("name").into_iter().collect()
            }
            "parameter_declaration" | "variadic_parameter_declaration" | "var_spec" | "const_spec" => {
                fields(node, "name")
            }
            "short_var_declaration" => left_names(node),
            "range_clause" | "receive_statement" if children(node).iter().any(|c| c.kind() == ":=") => left_names(node),
            "type_switch_statement" => node.child_by_field_name("alias").map(named).unwrap_or_default(),
            "import_spec" => {
                node.child_by_field_name("name").filter(|n| n.kind() == "package_identifier").into_iter().collect()
            }
            _ => vec![],
        };
        // A `:=`'s left side is expressions, which the go tool refuses unless they are names.
        let names = bound.into_iter().filter(|n| n.kind().ends_with("identifier") && text.of(*n) != "_");
        for name in names {
            let column = u32::try_from(name.start_position().column).unwrap_or(u32::MAX);
            out.push(Binding { name: text.of(name).into(), line: line(name), column, fixed: false });
        }
        pending.extend(named(node).into_iter().rev());
    }
    out
}

/// The expressions on the left of a `:=`, `node`.
fn left_names(node: Node) -> Vec<Node> {
    node.child_by_field_name("left").map(named).unwrap_or_default()
}
