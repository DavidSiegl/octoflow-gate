//! The flow of a Python function's body as the cognitive-complexity check scores it ([`model::Flow`]), and the
//! functions of its module that its calls reach, resolved through Python's scopes; docs/quality-gate.md, "Cognitive
//! complexity", has the rules.

use std::collections::{BTreeSet, HashSet};
use std::rc::Rc;

use model::Flow;
use tree_sitter::Node;

use crate::binds::{binds, declared, deleted, parameters, type_parameter_bounds, type_parameters, How};
use crate::syntax::{children, fields, is_trivia, named, of_kind, unparenthesized, Text};

/// The flow of `nodes`, statements or expressions of one function read in the order given, each with what it holds.
/// `owner` is the name of the class nested in the judged function whose body holds them directly, None outside one:
/// a method of it recurses by calling itself through `self`, `cls` or that name.
///
/// - `if`, a loop, `except`, `match` and a ternary are an increment that nests, with what they read before their
///   body at their own level; `elif` and `else` one that does not. A `case`'s pattern holds no flow.
/// - `try`, `finally` and `with` are no flow: their bodies stand in their place.
/// - A run of `and` and `or` is one [`Flow::Booleans`], an operand in parentheses that is itself such a run spliced
///   in, as Python's ast splices it; `not (a or b)` starts a run of its own.
/// - A lambda and a nested def nest their bodies, their defaults and decorators read at the level around; a nested
///   def's annotations and type parameters hold no flow that counts. A nested class is no level: its bases and its
///   body stand in its place, its methods nested defs.
pub fn flow(nodes: Vec<Node>, owner: Option<&str>, text: Text) -> Vec<Flow> {
    let within = |nodes: Vec<Node>| flow(nodes, owner, text);
    let part = |node: Node, field: &str| flow(fields(node, field), owner, text);
    let else_of = |node: Node| {
        let clauses = of_kind(node, "else_clause").into_iter();
        clauses.map(|e| Flow::Increment { nesting: false, level: vec![], nested: within(named(e)) }).collect::<Vec<_>>()
    };
    let mut out = vec![];
    // A stack for what breaks no flow, so that a sum of thousands of terms is not too deep; only the constructs
    // below recurse, which Python's own parser nests a hundred deep at most.
    let mut todo: Vec<Node> = nodes.into_iter().rev().collect();
    while let Some(node) = todo.pop() {
        match node.kind() {
            "comment" | "line_continuation" => {}
            "if_statement" => {
                let (level, nested) = (part(node, "condition"), part(node, "consequence"));
                out.push(Flow::Increment { nesting: true, level, nested });
                for elif in of_kind(node, "elif_clause") {
                    let (level, nested) = (part(elif, "condition"), part(elif, "consequence"));
                    out.push(Flow::Increment { nesting: false, level, nested });
                }
                out.extend(else_of(node));
            }
            "for_statement" | "while_statement" => {
                let head: Vec<Node> =
                    ["left", "right", "condition"].iter().flat_map(|field| fields(node, field)).collect();
                out.push(Flow::Increment { nesting: true, level: within(head), nested: part(node, "body") });
                out.extend(else_of(node));
            }
            "try_statement" => {
                for held in named(node) {
                    match held.kind() {
                        "except_clause" | "except_group_clause" => {
                            let (body, caught): (Vec<Node>, Vec<Node>) =
                                named(held).into_iter().partition(|n| n.kind() == "block");
                            out.push(Flow::Increment { nesting: true, level: within(caught), nested: within(body) });
                        }
                        "else_clause" => {}
                        _ => out.extend(within(vec![held])),
                    }
                }
                out.extend(else_of(node));
            }
            "match_statement" => {
                let cases = fields(node, "body").into_iter().flat_map(|body| of_kind(body, "case_clause"));
                let held: Vec<Node> = cases
                    .flat_map(|case| [fields(case, "guard"), fields(case, "consequence")].concat())
                    .collect();
                out.push(Flow::Increment { nesting: true, level: part(node, "subject"), nested: within(held) });
            }
            "conditional_expression" => {
                out.push(Flow::Increment { nesting: true, level: vec![], nested: within(named(node)) });
            }
            "boolean_operator" => {
                let (ands, operands) = boolean_run(node, text);
                out.push(Flow::Booleans { ands, operands: within(operands) });
            }
            "lambda" => {
                out.extend(within(defaults(node)));
                out.push(Flow::Nesting { recursive: false, body: part(node, "body") });
            }
            "function_definition" => {
                out.extend(within(defaults(node)));
                let name = node.child_by_field_name("name").map(|n| text.of(n)).unwrap_or_default();
                let key = (owner.map(String::from), name.to_string());
                let recursive = callees(node, owner, &BTreeSet::new(), text).contains(&key);
                out.push(Flow::Nesting { recursive, body: flow(fields(node, "body"), None, text) });
            }
            "class_definition" => {
                out.extend(part(node, "superclasses"));
                let name = node.child_by_field_name("name").map(|n| text.of(n));
                out.extend(flow(fields(node, "body"), name, text));
            }
            _ => todo.extend(children(node).into_iter().rev()),
        }
    }
    out
}

/// The default values of the parameters of `function`, a def or a lambda, which Python evaluates where the function
/// is defined.
fn defaults(function: Node) -> Vec<Node> {
    let parameters = function.child_by_field_name("parameters").map(named).unwrap_or_default();
    parameters.into_iter().filter_map(|p| p.child_by_field_name("value")).collect()
}

/// The operators of the run of `and` and `or` that `node`, a `boolean_operator`, heads, true for `and`, in the order
/// they are written, and the operands that are no such run themselves.
fn boolean_run<'a>(node: Node<'a>, text: Text) -> (Vec<bool>, Vec<Node<'a>>) {
    enum Item<'a> {
        Operand(Node<'a>),
        Operator(bool),
    }
    let (mut ands, mut operands) = (vec![], vec![]);
    // What is still to be read, the next item last.
    let mut todo = vec![Item::Operand(node)];
    while let Some(item) = todo.pop() {
        match item {
            Item::Operator(and) => ands.push(and),
            Item::Operand(operand) => {
                let inner = unparenthesized(operand);
                let sides = (inner.child_by_field_name("left"), inner.child_by_field_name("right"));
                match sides {
                    (Some(left), Some(right)) if inner.kind() == "boolean_operator" => {
                        let and = inner.child_by_field_name("operator").is_some_and(|o| text.of(o) == "and");
                        todo.extend([Item::Operand(right), Item::Operator(and), Item::Operand(left)]);
                    }
                    _ => operands.push(operand),
                }
            }
        }
    }
    (ands, operands)
}

/// A function of a module as a call names it: the qualified name of the class it is a method of, None outside one,
/// and its name.
pub type Callee = (Option<String>, String);

/// The functions of its module that the body of `function`, a def, calls, the bodies of the functions nested in it
/// included. `cls` is the name of the class `function` is a method of, None outside one, qualified as the classes in
/// the result are; `enclosing` are the type parameters of the classes around it, which its body sees.
///
/// Calls resolve by name, as Python's scopes do: `f()` calls the module's `f` unless a scope the call is read in binds
/// `f`, as a parameter, a local, a nested function or a type parameter, and declares it neither global nor nonlocal;
/// `C.m()` the method `m` of the module's class `C`, under the same proviso for `C`; and `self.m()` or `cls.m()` the
/// method `m` of `cls`, or in a class nested in `function`, of that class, which is no class of the module. A call of
/// anything else, such as an alias or an imported module's function, resolves to nothing.
pub fn callees<'a>(function: Node<'a>, cls: Option<&str>, enclosing: &BTreeSet<String>, text: Text) -> HashSet<Callee> {
    let mut scopes = Scopes { all: vec![] };
    let name = function.child_by_field_name("name").map(|n| text.of(n)).unwrap_or_default();
    let around = (!enclosing.is_empty()).then(|| scopes.open(None, Kind::Annotation, enclosing.iter().cloned()));
    let typed = scopes.of_type_parameters(function, around, text);
    let root = scopes.open(typed, Kind::Function, parameter_names(function, text));
    // Each node still to be read, with the scope it is read in and the class that self and cls name there. The
    // scopes' names are all known only once the walk is done, as Python fixes them at compile time, so the calls are
    // resolved after it.
    let owner: Option<Rc<str>> = cls.map(Rc::from);
    let mut todo: Vec<(Node, usize, Option<Rc<str>>)> =
        fields(function, "body").into_iter().map(|body| (body, root, owner.clone())).collect();
    let mut calls = vec![];
    while let Some((node, scope, owner)) = todo.pop() {
        if is_trivia(node) {
            continue;
        }
        if let Some(called) = node.child_by_field_name("function").filter(|_| node.kind() == "call") {
            calls.push((called, scope, owner.clone()));
        }
        scopes.bind(scope, node, text);
        let mut read = |nodes: Vec<Node<'a>>, scope: usize, owner: &Option<Rc<str>>| {
            todo.extend(nodes.into_iter().map(|n| (n, scope, owner.clone())));
        };
        match node.kind() {
            // A nested function's defaults are read here, its annotations and its type parameters' bounds in its type
            // parameters' scope, and its body in its own scope inside that; its decorators come before it, here.
            "function_definition" | "lambda" => {
                let typed = scopes.of_type_parameters(node, Some(scope), text).unwrap_or(scope);
                let inner = scopes.open(Some(typed), Kind::Function, parameter_names(node, text));
                let parameters = node.child_by_field_name("parameters").map(named).unwrap_or_default();
                read(parameters.iter().filter_map(|p| p.child_by_field_name("value")).collect(), scope, &owner);
                read(parameters.iter().filter_map(|p| p.child_by_field_name("type")).collect(), typed, &owner);
                read([fields(node, "return_type"), type_parameter_bounds(node)].concat(), typed, &owner);
                read(fields(node, "body"), inner, &owner);
            }
            // A nested class's bases and its type parameters' bounds are read in its type parameters' scope, and its
            // body in a class scope inside that, where self and cls name a class that is none of the module's.
            "class_definition" => {
                let typed = scopes.of_type_parameters(node, Some(scope), text).unwrap_or(scope);
                let members = scopes.open(Some(typed), Kind::Class, []);
                let class = node.child_by_field_name("name").map(|n| text.of(n)).unwrap_or_default();
                let marker: Option<Rc<str>> = Some(Rc::from(format!("<{class} in {name}>")));
                read([fields(node, "superclasses"), type_parameter_bounds(node)].concat(), typed, &owner);
                read(fields(node, "body"), members, &marker);
            }
            // A comprehension's first iterable is read here, as Python evaluates it before the comprehension starts;
            // everything else, its loop targets included, in the comprehension's own scope.
            "list_comprehension" | "set_comprehension" | "dictionary_comprehension" | "generator_expression" => {
                let inner = scopes.open(Some(scope), Kind::Comprehension, []);
                let first = of_kind(node, "for_in_clause").into_iter().next();
                for held in named(node) {
                    if Some(held) != first {
                        read(vec![held], inner, &owner);
                        continue;
                    }
                    let bound = binds(held, text).into_iter().filter(|b| b.how == How::Name);
                    scopes.all[inner].bound.extend(bound.map(|b| b.name.to_string()));
                    read(fields(held, "left"), inner, &owner);
                    read(fields(held, "right"), scope, &owner);
                }
            }
            "named_expression" => read(fields(node, "value"), scope, &owner),
            _ => read(children(node), scope, &owner),
        }
    }
    let resolved = |(called, scope, owner): (Node, usize, Option<Rc<str>>)| {
        scopes.callee(called, scope, owner.as_deref(), text)
    };
    calls.into_iter().filter_map(resolved).collect()
}

/// The names of the parameters of `function`, a def or a lambda.
fn parameter_names(function: Node, text: Text) -> Vec<String> {
    let parameters = function.child_by_field_name("parameters").map(parameters).unwrap_or_default();
    parameters.into_iter().map(|p| text.of(p).to_string()).collect()
}

/// What a [`Scope`] is the scope of.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// A function's or a lambda's body.
    Function,
    Class,
    Comprehension,
    /// A type-parameter list's, PEP 695's annotation scope, which unlike a function's sees the names of a class body
    /// it is directly in.
    Annotation,
}

/// A scope [`callees`] reads a function in, as Python's compiler sees it: the function's own; a function, lambda,
/// comprehension, class body or type-parameter list nested in it; or, around the function's own, its type-parameter
/// list and those of the classes it is a method of
/// (<https://docs.python.org/3/reference/executionmodel.html#resolution-of-names>).
struct Scope {
    /// The index in [`Scopes::all`] of the scope this one is nested in; None for the outermost, beyond which is the
    /// module.
    parent: Option<usize>,
    kind: Kind,
    /// The names it binds, and so means by itself.
    bound: HashSet<String>,
    /// The names it declares global, then nonlocal.
    globals: HashSet<String>,
    nonlocals: HashSet<String>,
}

/// The scopes of one [`callees`] walk, each named by its index.
struct Scopes {
    all: Vec<Scope>,
}

impl Scopes {
    /// A new scope of `kind` inside `parent` that binds `bound`; its index.
    fn open(&mut self, parent: Option<usize>, kind: Kind, bound: impl IntoIterator<Item = String>) -> usize {
        let bound = bound.into_iter().collect();
        self.all.push(Scope { parent, kind, bound, globals: HashSet::new(), nonlocals: HashSet::new() });
        self.all.len() - 1
    }

    /// The scope the type parameters of `definition`, a def, lambda or class, open inside `parent`, in which its
    /// annotations, bases and the parameters' bounds are read and its body's scope is nested; `parent` itself for a
    /// definition with none.
    fn of_type_parameters(&mut self, definition: Node, parent: Option<usize>, text: Text) -> Option<usize> {
        let names: Vec<String> = type_parameters(definition, text).into_iter().map(|b| b.name.to_string()).collect();
        if names.is_empty() {
            parent
        } else {
            Some(self.open(parent, Kind::Annotation, names))
        }
    }

    /// Takes in what `node`, read in `scope`, binds or declares there; nothing for any other node. A nested
    /// function's parameters and a class's members are their own scopes'.
    fn bind(&mut self, scope: usize, node: Node, text: Text) {
        for bound in binds(node, text) {
            match bound.how {
                How::Name => self.all[scope].bound.insert(bound.name.to_string()),
                How::Walrus => {
                    // Where an assignment expression binds its target: the nearest scope that is no comprehension's.
                    let mut at = scope;
                    while let (Kind::Comprehension, Some(parent)) = (self.all[at].kind, self.all[at].parent) {
                        at = parent;
                    }
                    self.all[at].bound.insert(bound.name.to_string())
                }
                How::Parameter | How::TypeParameter | How::SelfAttribute => false,
            };
        }
        self.all[scope].bound.extend(deleted(node, text).into_iter().map(String::from));
        let names = || declared(node, text).into_iter().map(String::from);
        match node.kind() {
            "global_statement" => self.all[scope].globals.extend(names()),
            "nonlocal_statement" => self.all[scope].nonlocals.extend(names()),
            _ => {}
        }
    }

    /// Whether `name`, read in `scope`, means the module's name rather than one bound in a scope of the chain: no
    /// scope from there out to the outermost binds it, unless one on the way declares it global. A class body counts
    /// only for a name read in it, or read in an annotation scope directly inside it, not in a scope nested in that.
    fn means_module(&self, scope: usize, name: &str) -> bool {
        let mut at = Some(scope);
        // The scope the walk came from, None at `scope`.
        let mut inner: Option<usize> = None;
        while let Some(k) = at {
            let here = &self.all[k];
            if here.globals.contains(name) {
                return true;
            }
            let sees_class = inner.is_none_or(|i| i == scope && self.all[i].kind == Kind::Annotation);
            if here.bound.contains(name) && !here.nonlocals.contains(name) && (here.kind != Kind::Class || sees_class) {
                return false;
            }
            (at, inner) = (here.parent, Some(k));
        }
        true
    }

    /// The function of the module that a call of `called`, read in `scope` where self and cls name the class
    /// `owner`, calls; None for a call of anything else.
    fn callee(&self, called: Node, scope: usize, owner: Option<&str>, text: Text) -> Option<Callee> {
        let called = unparenthesized(called);
        if called.kind() == "identifier" {
            let name = text.of(called);
            return self.means_module(scope, name).then(|| (None, name.to_string()));
        }
        let object = called.child_by_field_name("object").filter(|o| o.kind() == "identifier")?;
        let method = called.child_by_field_name("attribute").filter(|_| called.kind() == "attribute")?;
        let (object, method) = (text.of(object), text.of(method).to_string());
        match owner {
            Some(owner) if matches!(object, "self" | "cls") => Some((Some(owner.to_string()), method)),
            _ => self.means_module(scope, object).then(|| (Some(object.to_string()), method)),
        }
    }
}
