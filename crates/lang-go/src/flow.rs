//! The flow of a Go function's body, as the cognitive-complexity check weighs it ([`model::Flow`]), and the calls in
//! it that the file may resolve.

use model::Flow;
use tree_sitter::Node;

use crate::syntax::{named, Text};

/// A call a function's body makes that may reach a function of its own file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Call {
    /// `f(...)`: a function by its bare name.
    Plain(String),
    /// `r.m(...)`, `r` being the calling method's own receiver: a method of the caller's receiver type.
    OwnMethod(String),
}

/// The flow of a function's `body`, and each call in it as [`Call`] has them, those of the `func` literals it holds
/// included; `receiver` is the name of the method's receiver variable, None for a function or a method that names
/// none. Each Go construct is the [`Flow`] part of docs/quality-gate.md's "Cognitive complexity":
/// - an `if` an increment that nests, its initializer and condition at its level and its block nested; each `else if`
///   and `else` after it an increment that does not nest;
/// - a `for` of any form, an expression `switch`, a type switch and a `select` an increment that nests, its clauses
///   or its switch's initializer and value at its level and its cases nested; a case adds nothing;
/// - a `break` or `continue` to a label, and a `goto`, an increment that does not nest;
/// - a run of `&&` and `||`, through parentheses, a [`Flow::Booleans`];
/// - a `func` literal a [`Flow::Nesting`], never recursive: it has no name to call itself by.
pub fn flow(body: Node, receiver: Option<&str>, text: Text) -> (Vec<Flow>, Vec<Call>) {
    let mut walk = Walk { receiver, text, calls: vec![] };
    let flow = walk.part(body);
    (flow, walk.calls)
}

/// The walk [`flow`] makes.
struct Walk<'a> {
    receiver: Option<&'a str>,
    text: Text<'a>,
    /// The calls met so far, in the order they are written.
    calls: Vec<Call>,
}

impl Walk<'_> {
    /// The flow `node` holds, appended to `out`.
    fn node(&mut self, node: Node, out: &mut Vec<Flow>) {
        match node.kind() {
            "if_statement" => self.if_chain(node, out),
            "for_statement" => {
                let (body, head): (Vec<Node>, Vec<Node>) =
                    named(node).into_iter().partition(|c| node.child_by_field_name("body") == Some(*c));
                let level = self.parts(head);
                let nested = self.parts(body);
                out.push(Flow::Increment { nesting: true, level, nested });
            }
            "expression_switch_statement" | "type_switch_statement" | "select_statement" => {
                let (cases, head): (Vec<Node>, Vec<Node>) = named(node).into_iter().partition(|c| {
                    matches!(c.kind(), "expression_case" | "type_case" | "communication_case" | "default_case")
                });
                let level = self.parts(head);
                let nested = self.parts(cases);
                out.push(Flow::Increment { nesting: true, level, nested });
            }
            "break_statement" | "continue_statement" | "goto_statement" => {
                let labelled = named(node).iter().any(|c| c.kind() == "label_name");
                if labelled {
                    out.push(Flow::Increment { nesting: false, level: vec![], nested: vec![] });
                }
            }
            "binary_expression" if logical(node, self.text).is_some() => {
                let (mut ands, mut operands) = (vec![], vec![]);
                logical_run(node, self.text, &mut ands, &mut operands);
                let operands = self.parts(operands);
                out.push(Flow::Booleans { ands, operands });
            }
            "func_literal" => {
                let body = node.child_by_field_name("body").map(|b| self.part(b)).unwrap_or_default();
                out.push(Flow::Nesting { recursive: false, body });
            }
            "call_expression" | "type_conversion_expression" => {
                self.calls.extend(self.call(node));
                self.children(node, out);
            }
            _ => self.children(node, out),
        }
    }

    fn children(&mut self, node: Node, out: &mut Vec<Flow>) {
        for child in named(node) {
            self.node(child, out);
        }
    }

    /// The flow `node` holds, as one part of a node of flow.
    fn part(&mut self, node: Node) -> Vec<Flow> {
        let mut out = vec![];
        self.node(node, &mut out);
        out
    }

    /// The flow `nodes` hold, in order, as one part of a node of flow.
    fn parts(&mut self, nodes: Vec<Node>) -> Vec<Flow> {
        let mut out = vec![];
        for node in nodes {
            self.node(node, &mut out);
        }
        out
    }

    /// An `if`, with the `else if`s and the `else` that follow it, each an increment of its own at the `if`'s level.
    fn if_chain(&mut self, node: Node, out: &mut Vec<Flow>) {
        let mut node = node;
        let mut nesting = true;
        loop {
            let head = ["initializer", "condition"].iter().filter_map(|f| node.child_by_field_name(f)).collect();
            let level = self.parts(head);
            let nested = node.child_by_field_name("consequence").map(|c| self.part(c)).unwrap_or_default();
            out.push(Flow::Increment { nesting, level, nested });
            nesting = false;
            match node.child_by_field_name("alternative") {
                Some(otherwise) if otherwise.kind() == "if_statement" => node = otherwise,
                Some(otherwise) => {
                    let nested = self.part(otherwise);
                    out.push(Flow::Increment { nesting: false, level: vec![], nested });
                    return;
                }
                None => return,
            }
        }
    }

    /// The call `node` makes that may reach a function of the file: `f(...)`, `f[...](...)`, or a method of the
    /// receiver, `r.m(...)`; None for any other, as `pkg.F(...)` or `x.m(...)`. `f[...]` names `f` whether what is
    /// in its brackets are type arguments or an index, which tree-sitter cannot tell apart; and a conversion to a
    /// generic type, `g[int](x)`, reads as a call of `g`, as tree-sitter cannot tell it from a generic call either.
    fn call(&self, node: Node) -> Option<Call> {
        let text = self.text;
        let callee = match node.kind() {
            "call_expression" => node.child_by_field_name("function")?,
            _ => node.child_by_field_name("type").filter(|t| t.kind() == "generic_type")?,
        };
        let callee = match callee.kind() {
            "generic_type" => callee.child_by_field_name("type")?,
            "index_expression" => callee.child_by_field_name("operand")?,
            _ => callee,
        };
        match callee.kind() {
            "identifier" | "type_identifier" => Some(Call::Plain(text.of(callee).into())),
            "selector_expression" => {
                let operand = callee.child_by_field_name("operand")?;
                if operand.kind() != "identifier" || Some(text.of(operand)) != self.receiver {
                    return None;
                }
                Some(Call::OwnMethod(text.of(callee.child_by_field_name("field")?).into()))
            }
            _ => None,
        }
    }
}

/// Whether `node` is a `&&` or `||` and which: true for `&&`. None for any other node.
fn logical(node: Node, text: Text) -> Option<bool> {
    if node.kind() != "binary_expression" {
        return None;
    }
    match node.child_by_field_name("operator").map(|o| text.of(o)) {
        Some("&&") => Some(true),
        Some("||") => Some(false),
        _ => None,
    }
}

/// Appends to `ands` the operators of the run of `&&` and `||` that `node`, one of them, starts, through parentheses,
/// in the order they are written, and to `operands` what they join.
fn logical_run<'t>(node: Node<'t>, text: Text, ands: &mut Vec<bool>, operands: &mut Vec<Node<'t>>) {
    let (Some(left), Some(right)) = (node.child_by_field_name("left"), node.child_by_field_name("right")) else {
        return;
    };
    logical_operand(left, text, ands, operands);
    ands.push(logical(node, text) == Some(true));
    logical_operand(right, text, ands, operands);
}

fn logical_operand<'t>(node: Node<'t>, text: Text, ands: &mut Vec<bool>, operands: &mut Vec<Node<'t>>) {
    let mut inner = node;
    while inner.kind() == "parenthesized_expression" {
        match named(inner).into_iter().find(|c| c.kind() != "comment") {
            Some(i) => inner = i,
            None => break,
        }
    }
    if logical(inner, text).is_some() {
        logical_run(inner, text, ands, operands);
    } else {
        operands.push(node);
    }
}
