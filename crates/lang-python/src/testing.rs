//! What the test-integrity check reads of a Python test file: its tests and their assertions, the skips and
//! swallowed errors written in it, and the module-level constants its assertions can name.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use model::{first_line, Assertion, Constant, Lines, Test, TestGroup, Weakening, WeakeningKind};
use tree_sitter::Node;

use crate::binds::{binds, declared, deleted, How, SCOPES};
use crate::syntax::{
    children, decorated, digest, digest_of, end_line, fields, line, named, of_kind, on_one_line, tokens, undecorated,
    unparenthesized, Text, Token,
};

/// The last part of a name that skips a test or expects it to fail, in unittest and pytest.
const SKIP_NAMES: [&str; 8] =
    ["skip", "skipIf", "skipUnless", "skipif", "skipTest", "SkipTest", "xfail", "expectedFailure"];

/// The tests of the module `root`, and the classes that hold tests: a module-level def named `test*`; a method named
/// `test*` directly in a module-level class, as `Class.test_x`; and as groups, each module-level class with such a
/// method or named `Test*`. `spellings` are the module's own dotted names ([`module_spellings`]), by which an
/// assertion can name its constants.
pub fn tests(root: Node, spellings: &HashSet<String>, text: Text) -> (Vec<Test>, Vec<TestGroup>) {
    let is_test = |definition: &Node| {
        definition.kind() == "function_definition" && name(*definition, text).starts_with("test")
    };
    let test = |definition: Node, name: String| {
        let whole = decorated(definition);
        Test {
            name,
            line: line(definition),
            lines: Lines { first: line(whole), last: end_line(definition) },
            body: definition.child_by_field_name("body").map(|body| digest(body, text)).unwrap_or_default(),
            assertions: assertions(whole, spellings, text),
        }
    };
    let (mut tests, mut groups) = (vec![], vec![]);
    for definition in named(root).into_iter().map(undecorated) {
        if is_test(&definition) {
            tests.push(test(definition, name(definition, text).to_string()));
        } else if definition.kind() == "class_definition" {
            let class = name(definition, text);
            let body = definition.child_by_field_name("body").map(named).unwrap_or_default();
            let methods: Vec<Node> = body.into_iter().map(undecorated).filter(is_test).collect();
            if !methods.is_empty() || class.starts_with("Test") {
                let lines = Lines { first: line(decorated(definition)), last: end_line(definition) };
                groups.push(TestGroup { name: class.to_string(), line: line(definition), lines });
            }
            tests.extend(methods.into_iter().map(|m| test(m, format!("{class}.{}", name(m, text)))));
        }
    }
    (tests, groups)
}

/// The name of `definition`, a def or a class.
fn name<'t>(definition: Node, text: Text<'t>) -> &'t str {
    definition.child_by_field_name("name").map(|n| text.of(n)).unwrap_or_default()
}

/// The assertions of `test`, a test's def with its decorators, in the order they start: `assert` statements,
/// `self.assert*(...)` calls and `pytest.raises(...)` calls, anywhere in it.
fn assertions(test: Node, spellings: &HashSet<String>, text: Text) -> Vec<Assertion> {
    let own = names_bound_in(test, text);
    let mut found = vec![];
    let mut todo = vec![test];
    while let Some(node) = todo.pop() {
        let asserts = match node.kind() {
            "assert_statement" => true,
            "call" => {
                let called = node.child_by_field_name("function").and_then(|f| object_and_attribute(f, text));
                called.is_some_and(|(object, attribute)| match object {
                    "self" => attribute.starts_with("assert"),
                    "pytest" => attribute == "raises",
                    _ => false,
                })
            }
            _ => false,
        };
        if asserts {
            found.push(node);
        }
        todo.extend(named(node));
    }
    found.sort_by_key(|n| n.start_byte());
    found
        .into_iter()
        .map(|node| {
            let (shape, literals) = shape_and_literals(node, text);
            Assertion {
                line: line(node),
                text: on_one_line(text.of(node)),
                shape,
                literals,
                reads: reads(node, &own, spellings, text),
            }
        })
        .collect()
}

/// The kinds of node that [`shape_and_literals`] takes for one literal when every token in them is a literal or one
/// of [`CONSTANT_WORDS`]: a unary operator's, a parenthesized expression's, and those of a tuple, list, set or dict
/// display.
const CONSTANT_KINDS: [&str; 7] =
    ["unary_operator", "parenthesized_expression", "tuple", "list", "set", "dictionary", "pair"];

/// The words a node of [`CONSTANT_KINDS`] can hold and still be one literal: brackets, separators and the unary
/// operators. Within those kinds a word is one of a unary operator's, or punctuation; a binary operator is a node of
/// its own kind.
const CONSTANT_WORDS: [&str; 11] = ["(", ")", "[", "]", "{", "}", ",", ":", "-", "+", "~"];

/// A node whose tokens [`shape_and_literals`] is reading.
struct Open<'t> {
    kind: &'t str,
    /// Where its tokens start in the shape and in the literals.
    from: (usize, usize),
    /// Whether all its tokens so far may make it one literal; never for a kind outside [`CONSTANT_KINDS`].
    constant: bool,
    /// Its tokens so far, words and literals as written, each folded node among them as its one literal; only while
    /// `constant` holds, as they are read only then.
    written: Vec<String>,
}

impl Open<'_> {
    /// Adds `token`, as written, to the node's tokens, unless it can no longer be one literal.
    fn write(&mut self, token: &str) {
        if self.constant {
            self.written.push(token.into());
        }
    }
}

/// `assertion`'s shape and its literals, as model::Assertion holds them: [`tokens`], each literal `$lit` in the shape.
/// A value written as a constant expression is one literal, so that a change to its sign or to what holds it keeps
/// the shape: `-5`, `("a",)`, `{"k": [1, 2]}`, each a literal of its tokens as written, spaced, but for parentheses
/// around such a value, which change nothing and are left out of it. A node of [`CONSTANT_KINDS`] with a word other
/// than [`CONSTANT_WORDS`] in it, or a node of another kind, such as a name or an f-string, is no such value.
fn shape_and_literals(assertion: Node, text: Text) -> (String, Vec<String>) {
    let (mut shape, mut literals) = (vec![], vec![]);
    // The nodes opened and not yet closed, innermost last, below them the assertion's outside, never one literal.
    let mut open = vec![Open { kind: "", from: (0, 0), constant: false, written: vec![] }];
    tokens(assertion, text, &mut |token| {
        let innermost = open.last_mut().expect("the outside is never closed");
        match token {
            Token::Open(kind) => {
                let constant = CONSTANT_KINDS.contains(&kind);
                shape.push(format!("({kind}"));
                open.push(Open { kind, from: (shape.len() - 1, literals.len()), constant, written: vec![] });
            }
            Token::Word(word) => {
                shape.push(word.into());
                innermost.constant &= CONSTANT_WORDS.contains(&word);
                innermost.write(word);
            }
            Token::Literal(literal) => {
                shape.push("$lit".into());
                innermost.write(&literal);
                literals.push(literal);
            }
            Token::Close => {
                shape.push(")".into());
                let node = open.pop().expect("tokens closes only what it opened");
                let outer = open.last_mut().expect("the outside is never closed");
                if !node.constant {
                    outer.constant = false;
                    return;
                }
                let mut written = node.written;
                if node.kind == "parenthesized_expression" {
                    written.retain(|w| w != "(" && w != ")");
                }
                let literal = written.join(" ");
                shape.truncate(node.from.0);
                shape.push("$lit".into());
                literals.truncate(node.from.1);
                outer.write(&literal);
                literals.push(literal);
            }
        }
    });
    (shape.join(" "), literals)
}

/// (`a`, `b`) when `node` is the attribute `a.b` of a bare name.
fn object_and_attribute<'t>(node: Node, text: Text<'t>) -> Option<(&'t str, &'t str)> {
    let object = node.child_by_field_name("object").filter(|o| node.kind() == "attribute" && o.kind() == "identifier")?;
    Some((text.of(object), text.of(node.child_by_field_name("attribute")?)))
}

/// The names `assertion` reads as the module's, each once, in the order they are written: every name in it that its
/// test does not bind, `own` being those the test binds anywhere; and `NAME` of each `module.NAME` in it whose
/// `module` is one of `spellings` and starts at a name the test does not bind.
fn reads(assertion: Node, own: &HashSet<&str>, spellings: &HashSet<String>, text: Text) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    let mut read = |name: &str| {
        if !out.iter().any(|n| n == name) {
            out.push(name.into());
        }
    };
    let mut todo = vec![assertion];
    while let Some(node) = todo.pop() {
        match node.kind() {
            "identifier" if is_expression(node) && !own.contains(text.of(node)) => read(text.of(node)),
            "attribute" => {
                let object = node.child_by_field_name("object");
                let root = object.and_then(|o| dotted_root(o, text));
                let named_module = object.is_some_and(|o| spellings.contains(text.of(o)));
                if let Some(attribute) = node.child_by_field_name("attribute") {
                    if named_module && root.is_some_and(|root| !own.contains(root)) {
                        read(text.of(attribute));
                    }
                }
            }
            _ => {}
        }
        todo.extend(named(node).into_iter().rev());
    }
    out
}

/// The name `node` starts at when it is a name or a chain of attributes on one, written without spaces, as `a.b.c`.
fn dotted_root<'t>(mut node: Node, text: Text<'t>) -> Option<&'t str> {
    while node.kind() == "attribute" {
        node = node.child_by_field_name("object")?;
    }
    (node.kind() == "identifier").then(|| text.of(node))
}

/// Whether `identifier` is a name Python's ast reads as an expression: not an attribute's own name, a keyword
/// argument's, a parameter's, a def's or class's, a type parameter's ([`names_type_parameter`]), an import's, a
/// global's, an except's alias or a pattern's.
fn is_expression(identifier: Node) -> bool {
    let Some(parent) = identifier.parent() else { return true };
    let field = |name: &str| parent.child_by_field_name(name) == Some(identifier);
    let in_parameters =
        |node: Node| matches!(node.kind(), "parameters" | "lambda_parameters" | "typed_parameter");
    match parent.kind() {
        "attribute" => !field("attribute"),
        "keyword_argument" | "default_parameter" | "typed_default_parameter" | "function_definition"
        | "class_definition" => !field("name"),
        "parameters" | "lambda_parameters" | "dotted_name" | "aliased_import" | "global_statement"
        | "nonlocal_statement" | "splat_pattern" | "keyword_pattern" => false,
        // The name a match pattern captures comes last; what a with or an except reads comes first.
        "as_pattern" => parent.child(0) == Some(identifier),
        "typed_parameter" => named(parent).first() != Some(&identifier),
        "list_splat_pattern" | "dictionary_splat_pattern" => !parent.parent().is_some_and(in_parameters),
        "as_pattern_target" => {
            let holder = parent.parent().and_then(|as_pattern| as_pattern.parent());
            !holder.is_some_and(|h| matches!(h.kind(), "except_clause" | "except_group_clause"))
        }
        "type" | "splat_type" => !names_type_parameter(identifier),
        _ => true,
    }
}

/// Whether `identifier` is the name a type parameter declares, as `T`, `Ts` and `P` of `def f[T: int, *Ts, **P]()`,
/// of a def, a class or a type alias, where ast has no Name; not one in its bound, or in a subscript of a type, as
/// `list[T]`, which tree-sitter reads with the same nodes.
fn names_type_parameter(identifier: Node) -> bool {
    // Each parameter is a `type` in the `type_parameter` list: the name alone, a `splat_type` around it, or a
    // `constrained_type` whose first `type` is the name and whose second its bound.
    let declared = |ty: Node| ty.kind() == "type" && ty.parent().is_some_and(declares_type_parameters);
    let Some(parent) = identifier.parent() else { return false };
    let Some(above) = parent.parent() else { return false };
    match parent.kind() {
        "splat_type" => declared(above),
        "type" if above.kind() == "constrained_type" => {
            above.named_child(0) == Some(parent) && above.parent().is_some_and(declared)
        }
        _ => declared(parent),
    }
}

/// Whether `node` is a `type_parameter` list that declares its parameters: a def's or a class's, or the one after a
/// type alias's name, rather than a type's subscript.
fn declares_type_parameters(node: Node) -> bool {
    let Some(parent) = node.parent().filter(|_| node.kind() == "type_parameter") else { return false };
    if parent.child_by_field_name("type_parameters") == Some(node) {
        return true;
    }
    // `type A[T] = …` is read as a generic_type `A[T]` that is the alias's left side.
    let alias = parent.parent().filter(|ty| ty.kind() == "type").and_then(|ty| Some((ty, ty.parent()?)));
    parent.kind() == "generic_type"
        && alias.is_some_and(|(ty, statement)| {
            statement.kind() == "type_alias_statement" && statement.child_by_field_name("left") == Some(ty)
        })
}

/// Every name `node` binds anywhere inside it, whatever the scope: by assignment of any kind, as a parameter of it
/// or of a def or lambda in it, by import, as a for, with, except, comprehension or match target, by def or class, or
/// by naming it in global, nonlocal or del. An attribute assigned to binds no name.
fn names_bound_in<'t>(node: Node, text: Text<'t>) -> HashSet<&'t str> {
    let mut out = HashSet::new();
    let mut todo = vec![node];
    while let Some(node) = todo.pop() {
        out.extend(binds(node, text).into_iter().filter(|b| b.how != How::SelfAttribute).map(|b| b.name));
        out.extend(deleted(node, text));
        if matches!(node.kind(), "global_statement" | "nonlocal_statement") {
            out.extend(declared(node, text));
        }
        todo.extend(named(node));
    }
    out
}

/// The code that `node`, the name of a skip, is written in, whole: the decorator or call it names, with its
/// arguments, or `node` itself where it is neither's; and where that is passed in a call's arguments, as a mark in
/// `pytest.param(2, marks=[pytest.mark.xfail])`, that call, which tells the parametrized case it marks.
fn written_whole(node: Node) -> Node {
    let mut whole = node;
    while let Some(parent) = whole.parent() {
        let named_call = parent.kind() == "call" && parent.child_by_field_name("function") == Some(whole);
        if !(named_call || parent.kind() == "decorator") {
            break;
        }
        whole = parent;
    }
    let mut passed = whole;
    while let Some(parent) = passed.parent() {
        match parent.kind() {
            "keyword_argument" | "list" | "tuple" => passed = parent,
            "argument_list" => return parent.parent().unwrap_or(whole),
            _ => break,
        }
    }
    whole
}

/// What weakens a test in the test module `root`: each name or attribute that skips a test or expects it to fail
/// ([`SKIP_NAMES`]), once per code it is written in ([`written_whole`]), so that each parametrized case's marks are
/// one however they are wrapped; and each `except` that swallows what it catches ([`swallows`]), its lines those of
/// its head, up to the colon.
pub fn weakenings(root: Node, text: Text) -> Vec<Weakening> {
    let mut out = vec![];
    // The ids of the nodes `written_whole` gave for the skips read so far.
    let mut seen: HashSet<usize> = HashSet::new();
    let mut todo = vec![root];
    while let Some(node) = todo.pop() {
        let skip = match node.kind() {
            "identifier" if is_expression(node) => Some(text.of(node)),
            "attribute" => node.child_by_field_name("attribute").map(|a| text.of(a)),
            _ => None,
        };
        let whole = skip.filter(|name| SKIP_NAMES.contains(name)).map(|_| written_whole(node));
        if let Some(whole) = whole.filter(|whole| seen.insert(whole.id())) {
            out.push(Weakening {
                lines: Lines { first: line(whole), last: end_line(whole) },
                kind: WeakeningKind::Skip,
                code: digest(whole, text),
                message: format!("`{}` added: it skips a test or expects it to fail.", first_line(text.of(node))),
            });
        }
        if matches!(node.kind(), "except_clause" | "except_group_clause") && swallows(node, text) {
            // The clause up to the colon that opens its body.
            let mut head: Vec<Node> = children(node);
            head.truncate(head.iter().position(|c| c.kind() == ":").map_or(head.len(), |colon| colon + 1));
            let end = head.last().map_or(node.start_byte(), |c| c.end_byte());
            let written = text.0.get(node.start_byte()..end).unwrap_or_default();
            out.push(Weakening {
                lines: Lines { first: line(node), last: head.last().map_or(line(node), |c| end_line(*c)) },
                kind: WeakeningKind::SwallowedError,
                code: digest_of(&head, text),
                message: format!("`{}` added in a test: it hides the failure it catches.", first_line(written)),
            });
        }
        todo.extend(named(node).into_iter().rev());
    }
    out.sort_by_key(|w| w.lines.first);
    out
}

/// Whether the `except` clause `handler` hides the failure it catches: a bare except, or one catching `Exception` or
/// `BaseException` by that name whose body does nothing, only `pass` and literals.
fn swallows(handler: Node, text: Text) -> bool {
    let (body, caught): (Vec<Node>, Vec<Node>) = named(handler).into_iter().partition(|n| n.kind() == "block");
    let Some(caught) = caught.first() else { return true };
    // `except E as e` holds E as the first part of an as_pattern.
    let caught = match caught.kind() {
        "as_pattern" => named(*caught).into_iter().next().unwrap_or(*caught),
        _ => *caught,
    };
    let caught = unparenthesized(caught);
    let catches_all = caught.kind() == "identifier" && matches!(text.of(caught), "Exception" | "BaseException");
    let does_nothing = |statement: Node| match statement.kind() {
        "pass_statement" => true,
        "expression_statement" => match named(statement).as_slice() {
            [only] => is_literal(*only),
            _ => false,
        },
        _ => false,
    };
    catches_all && body.into_iter().flat_map(named).all(does_nothing)
}

/// Whether `node` is a constant as Python's ast has one: a number, a string without replacement fields, `True`,
/// `False`, `None` or `...`.
fn is_literal(node: Node) -> bool {
    let plain = |string: Node| of_kind(string, "interpolation").is_empty();
    match node.kind() {
        "integer" | "float" | "true" | "false" | "none" | "ellipsis" => true,
        "string" => plain(node),
        "concatenated_string" => named(node).into_iter().all(plain),
        _ => false,
    }
}

/// How many times the scope whose code is `nodes`, a module's statements, binds each name. A def's or class's name
/// counts, but what it binds inside does not, nor what a lambda or a comprehension binds, a `:=` in a comprehension
/// included, though Python binds that in the enclosing scope; an attribute assigned to binds no name.
fn scope_bindings<'t>(nodes: Vec<Node>, text: Text<'t>) -> HashMap<&'t str, usize> {
    let mut out: HashMap<&str, usize> = HashMap::new();
    let mut todo = nodes;
    while let Some(node) = todo.pop() {
        let opens_scope = SCOPES.contains(&node.kind());
        for bound in binds(node, text) {
            if bound.how == How::Name || (!opens_scope && bound.how != How::SelfAttribute) {
                *out.entry(bound.name).or_default() += 1;
            }
        }
        if !opens_scope {
            todo.extend(named(node));
        }
    }
    out
}

/// The constants of the module `root`: each name that one module-level assignment, annotated or not, binds to an
/// expression holding no call, and that nothing else in the module's own scope binds. Out of scope: a constant
/// imported from another module or computed by a call, whose value the file does not spell out, and one built from
/// other constants, whose edits this does not see.
pub fn constants(root: Node, text: Text) -> Vec<Constant> {
    let bindings = scope_bindings(named(root), text);
    let mut out = vec![];
    for statement in named(root).into_iter().filter(|s| s.kind() == "expression_statement") {
        let [assignment] = named(statement)[..] else { continue };
        let sides = (assignment.child_by_field_name("left"), assignment.child_by_field_name("right"));
        let (Some(left), Some(right)) = sides else { continue };
        // `a = b = 1` holds a second assignment as its right side: two targets, and no constant.
        if assignment.kind() != "assignment" || left.kind() != "identifier" || right.kind() == "assignment" {
            continue;
        }
        if bindings.get(text.of(left)) == Some(&1) && !holds_call(right) {
            out.push(Constant { name: text.of(left).into(), line: line(statement), expression: digest(right, text) });
        }
    }
    out
}

/// Whether `node` is or holds a call.
fn holds_call(node: Node) -> bool {
    let mut todo = vec![node];
    while let Some(node) = todo.pop() {
        if node.kind() == "call" {
            return true;
        }
        todo.extend(named(node));
    }
    false
}

/// The dotted names a test of the module `root`, the file at `path` from the tree's top level, can reach the module
/// itself by, to name a constant as `module.NAME`: the module's dotted name and each suffix of it that ends at the
/// file's own name, and the name a module-level import statement of one binds; a bare name only when nothing else in
/// the module binds it.
pub fn module_spellings(root: Node, path: &Path, text: Text) -> HashSet<String> {
    let path = path.with_extension("");
    let parts: Vec<String> = path.iter().map(|p| p.to_string_lossy().into_owned()).collect();
    let mut spellings: HashSet<String> = (0..parts.len()).map(|k| parts[k..].join(".")).collect();
    for (module, name, alias) in imports(root, text) {
        let bound = match (module, alias) {
            // `import a.b as c` binds c to a.b; without `as` it binds a, which is no new spelling.
            (None, Some(alias)) if spellings.contains(&name) => alias.to_string(),
            (Some(module), alias) if spellings.contains(&format!("{module}.{name}")) => {
                alias.map_or(name, str::to_string)
            }
            _ => continue,
        };
        spellings.insert(bound);
    }
    let bindings = scope_bindings(named(root), text);
    spellings.retain(|n| n.contains('.') || bindings.get(n.as_str()).is_none_or(|&times| times <= 1));
    spellings
}

/// What the module-level import statements of the module `root` import, in the order written, each as the module a
/// `from` statement names (None for an `import` statement), the dotted name imported, and the name after `as`. The
/// dotted names are without the spaces Python allows around their dots. A `from` statement that names its module
/// relatively, by leading dots, is left out.
fn imports<'t>(root: Node, text: Text<'t>) -> Vec<(Option<String>, String, Option<&'t str>)> {
    let dotted = |node: Node| text.of(node).split_whitespace().collect::<String>();
    let mut out = vec![];
    for statement in named(root) {
        let module = statement.child_by_field_name("module_name").filter(|m| m.kind() == "dotted_name");
        let module = match (statement.kind(), module) {
            ("import_statement", _) => None,
            ("import_from_statement", Some(module)) => Some(dotted(module)),
            _ => continue,
        };
        for imported in fields(statement, "name") {
            // An `aliased_import` holds the name and the alias; any other import is the name itself.
            let name = imported.child_by_field_name("name").unwrap_or(imported);
            let alias = imported.child_by_field_name("alias").map(|a| text.of(a));
            out.push((module.clone(), dotted(name), alias));
        }
    }
    out
}
