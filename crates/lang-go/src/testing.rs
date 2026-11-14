//! The tests of a `_test.go` file, their assertions, and what in the file weakens them; and the constants of a Go
//! file that its tests' assertions can name.

use std::collections::HashSet;

use model::{first_line, on_one_line, Assertion, Constant, Lines, Test, Weakening, WeakeningKind};
use tree_sitter::Node;

use crate::syntax::{
    children, digest, digest_of, doc_line, end_line, fields, function_name, is_literal, line, named, tokens, Text,
};
use crate::{binds, constraint};

/// The methods of `testing.T`, `B` and `F` that fail the test they are called on.
const FAILING: &[&str] = &["Error", "Errorf", "Fatal", "Fatalf", "Fail", "FailNow"];
/// The methods of `testing.T`, `B` and `F` that skip the test they are called on.
const SKIPPING: &[&str] = &["Skip", "Skipf", "SkipNow"];
/// The import paths of testify's packages of assertions.
const TESTIFY: &[&str] = &["github.com/stretchr/testify/assert", "github.com/stretchr/testify/require"];

/// The tests of `root`, a `_test.go` file's tree, in the order they start, and what weakens them, in the order
/// written.
///
/// - A test is a function `TestX(*testing.T)`, `BenchmarkX(*testing.B)` or `FuzzX(*testing.F)`, as `go test` finds
///   them, X empty or not starting with a lowercase letter; and a subtest, a `t.Run("name", func(...) {...})` whose
///   name is a string literal, named after the test holding it as `go test` names it, `TestX/name`, a space in the
///   literal written `_`. A `t.Run` named by any other expression, as a table-driven test's, is part of the test
///   holding it. An example, `ExampleX()`, is a test too where `go test` runs it, as [`example_output`] says, its one
///   assertion its output comment.
/// - Its assertions are those in its body but its subtests', in the order written: a call of testify's `assert` or
///   `require` (`assert.Equal(t, 3, got)`, by the names the file imports them under, `assert.New` not one), or of a
///   method of a name that a `:=` or a `var` in the same top-level function binds to such a `New` (`a :=
///   assert.New(t)`, then `a.Equal(3, got)`), whatever the scope, a name it is copied to not followed; and a
///   call of `Error`, `Errorf`, `Fatal`, `Fatalf`, `Fail` or `FailNow` on a variable that a parameter of type
///   `*testing.T`, `*testing.B`, `*testing.F` or `testing.TB` names, of a function or a `func` literal. Such a call
///   that is a statement of an `if`'s block is that `if`'s assertion, the `if` counting once whatever number of them
///   it holds: its text the `if` to the end of its block, and its literals and shape those of its initializer and
///   condition and of those calls, so that `if got != 3 { t.Errorf(...) }` changing its 3 is a changed expected
///   value. A call, by its name, of a helper of the file, as `check(t, Add(1, 2), 3)`, with such a variable among its
///   arguments, is one too, its literals the call's: a helper is a top-level function that holds such a failing call
///   or a testify assertion outside any test, or calls another helper so. A literal is a number, a rune or a string.
/// - A table of cases in its body, a composite literal that a `for … range` in the same top-level function ranges
///   over, written there or by a name a `:=` or a `var` there binds to it, as `[]struct{ in, want int }{{in: 2, want:
///   4}}`, holds its expected values: each of its elements written in braces of their own, its key with it in a
///   map's, is an assertion too, in the order written, its literals and shape its own and its text on one line
///   ([`on_one_line`]). A literal in a case is part of that case.
/// - An assertion reads ([`Assertion::reads`]) each name in it that the top-level function holding it binds nowhere
///   ([`binds::bindings`]), but a field's name keying an element of a literal not of a map type, as `want` in
///   `{want: 4}`.
/// - A weakening is a `Skip`, `Skipf` or `SkipNow` called on such a variable anywhere in the file; a build constraint
///   line above its `package` clause ([`constraint::weakenings`]); and, anywhere in the file, an error thrown away by
///   `_ = err` or by an `if err != nil {}` whose block holds no statement, `err` being any name of an error by Go's
///   habit: `err`, `err` then a capital or a digit, as `errClose`, or a name ending in `Err`. A parser knows no types,
///   so an error thrown away under any other name, or as `v, _ := f()`, is not seen.
pub fn read(root: Node, text: Text) -> (Vec<Test>, Vec<Weakening>) {
    let imports = Imports::of(root, text);
    let mut walk = Walk {
        text,
        imports,
        tests: vec![],
        weakenings: vec![],
        scope: vec![],
        owner: None,
        consumed: HashSet::new(),
        function: String::new(),
        own: HashSet::new(),
        tables: HashSet::new(),
        assertors: HashSet::new(),
        helpers: HashSet::new(),
        helper_calls: vec![],
    };
    let header: Vec<(&str, u32)> = named(root)
        .into_iter()
        .filter(|n| n.kind() == "comment" && n.end_byte() <= package_start(root))
        .map(|n| (text.of(n), line(n)))
        .collect();
    walk.weakenings.extend(constraint::weakenings(&header));
    for node in named(root) {
        if matches!(node.kind(), "function_declaration" | "method_declaration") {
            walk.function(node);
        }
    }
    walk.keep_helper_calls();
    (walk.tests, walk.weakenings)
}

/// Where the file's `package` clause starts; the file's end where it has none.
fn package_start(root: Node) -> usize {
    named(root).into_iter().find(|n| n.kind() == "package_clause").map_or(root.end_byte(), |p| p.start_byte())
}

/// The names a file imports the packages a test is written with under.
struct Imports {
    /// The name of the `testing` package; None where the file does not import it by a name.
    testing: Option<String>,
    /// The names of testify's `assert` and `require` packages.
    testify: Vec<String>,
}

impl Imports {
    fn of(root: Node, text: Text) -> Imports {
        let mut imports = Imports { testing: None, testify: vec![] };
        let declarations = named(root).into_iter().filter(|n| n.kind() == "import_declaration");
        let specs = declarations.flat_map(|d| {
            named(d).into_iter().flat_map(|s| if s.kind() == "import_spec_list" { named(s) } else { vec![s] })
        });
        for spec in specs.filter(|s| s.kind() == "import_spec") {
            let Some(path) = spec.child_by_field_name("path") else { continue };
            let path = text.of(path).trim_matches(['"', '`']);
            let name = match spec.child_by_field_name("name") {
                Some(n) if n.kind() == "package_identifier" => text.of(n),
                Some(_) => continue,
                None => path.rsplit('/').next().unwrap_or(path),
            };
            if path == "testing" {
                imports.testing = Some(name.into());
            } else if TESTIFY.contains(&path) {
                imports.testify.push(name.into());
            }
        }
        imports
    }

    /// The letter of the testing type a parameter's `ty` names: `T`, `B` or `F` for a pointer to `testing.T`, `B` or
    /// `F`, and `TB` for `testing.TB`; None for any other type.
    fn testing_type<'t>(&self, ty: Node, text: Text<'t>) -> Option<&'t str> {
        let (pointer, ty) = match ty.kind() {
            "pointer_type" => (true, ty.named_child(0)?),
            _ => (false, ty),
        };
        if ty.kind() != "qualified_type" {
            return None;
        }
        let package = text.of(ty.child_by_field_name("package")?);
        let name = text.of(ty.child_by_field_name("name")?);
        let fits = if pointer { matches!(name, "T" | "B" | "F") } else { name == "TB" };
        (fits && Some(package) == self.testing.as_deref()).then_some(name)
    }
}

/// The walk [`read`] makes.
struct Walk<'t> {
    text: Text<'t>,
    imports: Imports,
    tests: Vec<Test>,
    weakenings: Vec<Weakening>,
    /// The names of the testing variables in scope, the parameters of a testing type of the functions and `func`
    /// literals the walk is in.
    scope: Vec<&'t str>,
    /// The index in `tests` of the test whose assertions the walk is in; None outside a test.
    owner: Option<usize>,
    /// The failing calls already counted as the assertion of the `if` that holds them, by their nodes' ids.
    consumed: HashSet<usize>,
    /// The name of the top-level function the walk is in, as a finding names it.
    function: String,
    /// The names the top-level function the walk is in binds anywhere, which its assertions do not read as the file's.
    own: HashSet<String>,
    /// The tables of cases ([`read`]) of the top-level function the walk is in, by their nodes' ids.
    tables: HashSet<usize>,
    /// The names the top-level function the walk is in binds to testify's `New`, whose methods are assertions
    /// ([`read`]).
    assertors: HashSet<&'t str>,
    /// The names of the top-level functions met so far that are helpers ([`read`]) by a failing call of their own;
    /// [`Walk::keep_helper_calls`] adds those that are helpers by calling one.
    helpers: HashSet<String>,
    /// Each call met of a function by its name with a testing variable among its arguments, as a helper is called,
    /// where it was met, with the name of the function called.
    helper_calls: Vec<(Caller, &'t str)>,
}

/// Where a call [`Walk::helper_calls`] holds was met.
enum Caller {
    /// In a test: its index in [`Walk::tests`], and the index among its assertions of the one the call was taken for.
    Test(usize, usize),
    /// In the top-level function of this name, outside any test.
    Function(String),
}

impl<'t> Walk<'t> {
    /// A top-level function or method, `node`: a test when it is one, then its body.
    fn function(&mut self, node: Node<'t>) {
        let Some(body) = node.child_by_field_name("body") else { return };
        self.function = function_name(node, self.text);
        self.own = binds::bindings(node, self.text).into_iter().map(|b| b.name).collect();
        let bound = named_initializers(body, self.text);
        self.tables = ranged_literals(body, &bound, self.text);
        let assertors = bound.iter().filter(|(_, initializer)| self.testify_new(*initializer));
        self.assertors = assertors.map(|(name, _)| *name).collect();
        let params = self.testing_params(node);
        self.owner = None;
        let output = example_output(node, self.text);
        if node.kind() == "function_declaration" && (self.is_test(node) || output.is_some()) {
            self.owner = Some(self.tests.len());
            self.tests.push(Test {
                name: self.function.clone(),
                line: line(node),
                lines: Lines { first: doc_line(node).unwrap_or(line(node)), last: end_line(node) },
                body: digest(body, self.text),
                assertions: output.into_iter().collect(),
            });
        }
        let depth = self.scope.len();
        self.scope.extend(params);
        self.node(body);
        self.scope.truncate(depth);
    }

    /// Whether the function declared by `node` is a test `go test` runs: by its name, `TestX`, `BenchmarkX` or
    /// `FuzzX`, and its one parameter, of the testing type its name asks for.
    fn is_test(&self, node: Node) -> bool {
        let text = self.text;
        let name = node.child_by_field_name("name").map(|n| text.of(n)).unwrap_or_default();
        let wanted = [("Test", "T"), ("Benchmark", "B"), ("Fuzz", "F")].into_iter().find_map(|(prefix, ty)| {
            let rest = name.strip_prefix(prefix)?;
            (!rest.starts_with(char::is_lowercase)).then_some(ty)
        });
        let Some(wanted) = wanted else { return false };
        if node.child_by_field_name("type_parameters").is_some() || node.child_by_field_name("result").is_some() {
            return false;
        }
        let params = node.child_by_field_name("parameters").map(named).unwrap_or_default();
        let params: Vec<Node> = params.into_iter().filter(|p| p.kind() != "comment").collect();
        match params.as_slice() {
            [param]
                if param.kind() == "parameter_declaration"
                    && param.children_by_field_name("name", &mut param.walk()).count() <= 1 =>
            {
                param.child_by_field_name("type").and_then(|ty| self.imports.testing_type(ty, text)) == Some(wanted)
            }
            _ => false,
        }
    }

    /// The names of the parameters of a testing type of the function or `func` literal `node`.
    fn testing_params(&self, node: Node<'t>) -> Vec<&'t str> {
        let text = self.text;
        let params = node.child_by_field_name("parameters").map(named).unwrap_or_default();
        let typed = params
            .into_iter()
            .filter(|p| p.child_by_field_name("type").is_some_and(|ty| self.imports.testing_type(ty, text).is_some()));
        typed
            .flat_map(|p| p.children_by_field_name("name", &mut p.walk()).map(|n| text.of(n)).collect::<Vec<_>>())
            .collect()
    }

    fn node(&mut self, node: Node<'t>) {
        match node.kind() {
            "func_literal" => {
                let depth = self.scope.len();
                self.scope.extend(self.testing_params(node));
                self.children(node);
                self.scope.truncate(depth);
            }
            "call_expression" => {
                if self.subtest(node) {
                    return;
                }
                self.call(node);
                self.children(node);
            }
            "if_statement" => {
                self.if_statement(node);
                self.children(node);
            }
            "assignment_statement" => {
                self.discard(node);
                self.children(node);
            }
            "composite_literal" if self.tables.contains(&node.id()) => {
                self.table(node);
                self.children(node);
            }
            _ => self.children(node),
        }
    }

    fn children(&mut self, node: Node<'t>) {
        for child in named(node) {
            self.node(child);
        }
    }

    /// The method a call, `node`, calls on a testing variable in scope, as `Errorf` for `t.Errorf(...)`; None for
    /// any other call.
    fn testing_method(&self, node: Node<'t>) -> Option<&'t str> {
        let callee = node.child_by_field_name("function").filter(|f| f.kind() == "selector_expression")?;
        let operand = callee.child_by_field_name("operand").filter(|o| o.kind() == "identifier")?;
        if !self.scope.contains(&self.text.of(operand)) {
            return None;
        }
        Some(self.text.of(callee.child_by_field_name("field")?))
    }

    /// Whether the call `node` is a testify assertion: a function of a package the file imports testify's `assert`
    /// or `require` under, but `New`, or a method of one of [`Walk::assertors`].
    fn testify(&self, node: Node) -> bool {
        let Some((operand, field)) = self.selector_call(node) else { return false };
        (self.imports.testify.iter().any(|n| n == operand) && field != "New") || self.assertors.contains(operand)
    }

    /// Whether `node` is a call of testify's `New`, under a name the file imports `assert` or `require` by.
    fn testify_new(&self, node: Node) -> bool {
        let new = |(operand, field): (&str, &str)| field == "New" && self.imports.testify.iter().any(|n| n == operand);
        self.selector_call(node).is_some_and(new)
    }

    /// The name and the selected field of a call, `node`, of `name.field`; None for any other node.
    fn selector_call(&self, node: Node) -> Option<(&'t str, &'t str)> {
        if node.kind() != "call_expression" {
            return None;
        }
        let callee = node.child_by_field_name("function").filter(|f| f.kind() == "selector_expression")?;
        let operand = callee.child_by_field_name("operand").filter(|o| o.kind() == "identifier")?;
        Some((self.text.of(operand), self.text.of(callee.child_by_field_name("field")?)))
    }

    /// A subtest, when the call `node` starts one, `t.Run("name", func...)` in a test: records it and walks its
    /// literal with it the owner of the assertions met. Whether it was one.
    fn subtest(&mut self, node: Node<'t>) -> bool {
        let Some(parent) = self.owner else { return false };
        if self.testing_method(node) != Some("Run") {
            return false;
        }
        let args: Vec<Node> = node.child_by_field_name("arguments").map(named).unwrap_or_default();
        let args: Vec<Node> = args.into_iter().filter(|a| a.kind() != "comment").collect();
        let [name, literal] = args.as_slice() else { return false };
        let named_by_literal = matches!(name.kind(), "interpreted_string_literal" | "raw_string_literal");
        let Some(body) =
            literal.child_by_field_name("body").filter(|_| named_by_literal && literal.kind() == "func_literal")
        else {
            return false;
        };
        let written = self.text.of(*name);
        let own = written.get(1..written.len().saturating_sub(1)).unwrap_or_default().replace(' ', "_");
        self.owner = Some(self.tests.len());
        self.tests.push(Test {
            name: format!("{}/{own}", self.tests[parent].name),
            line: line(node),
            lines: Lines { first: line(node), last: end_line(node) },
            body: digest(body, self.text),
            assertions: vec![],
        });
        self.node(*literal);
        self.owner = Some(parent);
        true
    }

    /// A call, `node`, that is not a subtest: an assertion of the test the walk is in, or a skip.
    fn call(&mut self, node: Node<'t>) {
        let method = self.testing_method(node);
        if method.is_some_and(|m| SKIPPING.contains(&m)) {
            self.weakenings.push(Weakening {
                lines: Lines { first: line(node), last: end_line(node) },
                kind: WeakeningKind::Skip,
                code: digest(node, self.text),
                message: format!("`{}` added: it skips a test.", first_line(self.text.of(node))),
            });
        }
        let failing = method.is_some_and(|m| FAILING.contains(&m)) && !self.consumed.contains(&node.id());
        if failing || self.testify(node) {
            let assertion = self.assertion(node, &[node], node.end_byte());
            self.fail(assertion);
        } else if let Some(callee) = self.passing_testing(node) {
            let caller = match self.owner {
                Some(test) => {
                    let assertion = self.assertion(node, &[node], node.end_byte());
                    self.assert(assertion);
                    Caller::Test(test, self.tests[test].assertions.len() - 1)
                }
                None => Caller::Function(self.function.clone()),
            };
            self.helper_calls.push((caller, callee));
        }
    }

    /// The name of the function a call, `node`, calls by its name with a testing variable in scope among its
    /// arguments, as a helper is called; None for any other call.
    fn passing_testing(&self, node: Node<'t>) -> Option<&'t str> {
        let callee = node.child_by_field_name("function").filter(|f| f.kind() == "identifier")?;
        let args = node.child_by_field_name("arguments").map(named).unwrap_or_default();
        let passed = |a: &Node| a.kind() == "identifier" && self.scope.contains(&self.text.of(*a));
        args.iter().any(passed).then(|| self.text.of(callee))
    }

    /// An assertion that fails the test it is in: the test's; outside any test, what makes the top-level function the
    /// walk is in a helper.
    fn fail(&mut self, assertion: Assertion) {
        if self.owner.is_some() {
            self.assert(assertion);
        } else {
            self.helpers.insert(self.function.clone());
        }
    }

    /// Adds to [`Walk::helpers`] each function that calls one, then drops from the tests the assertions taken for
    /// calls of a function that is no helper.
    fn keep_helper_calls(&mut self) {
        loop {
            let calling = self.helper_calls.iter().filter_map(|(caller, callee)| match caller {
                Caller::Function(name) if self.helpers.contains(*callee) && !self.helpers.contains(name) => Some(name),
                _ => None,
            });
            let found: Vec<String> = calling.cloned().collect();
            if found.is_empty() {
                break;
            }
            self.helpers.extend(found);
        }
        // In reverse, so that each index still points at the assertion it was taken for.
        for (caller, callee) in self.helper_calls.iter().rev() {
            match caller {
                Caller::Test(test, k) if !self.helpers.contains(*callee) => {
                    self.tests[*test].assertions.remove(*k);
                }
                _ => {}
            }
        }
    }

    /// A table of cases, `node` ([`read`]): each case an assertion of the test the walk is in.
    fn table(&mut self, node: Node<'t>) {
        let elements = node.child_by_field_name("body").map(named).unwrap_or_default();
        for case in elements.into_iter().filter(|e| is_case(*e)) {
            let mut assertion = self.assertion(case, &[case], case.end_byte());
            assertion.text = on_one_line(&assertion.text);
            self.assert(assertion);
        }
    }

    fn assert(&mut self, assertion: Assertion) {
        if let Some(owner) = self.owner {
            self.tests[owner].assertions.push(assertion);
        }
    }

    /// An `if`, `node`: the assertion it is when its block holds failing calls as statements, and a swallowed error
    /// when it is `if err != nil {}`.
    fn if_statement(&mut self, node: Node<'t>) {
        let Some(block) = node.child_by_field_name("consequence") else { return };
        let statements: Vec<Node> = named(block)
            .into_iter()
            .filter(|s| s.kind() == "statement_list")
            .flat_map(named)
            .filter(|s| s.kind() != "comment")
            .collect();
        if statements.is_empty() && node.child_by_field_name("condition").is_some_and(|c| self.error_not_nil(c)) {
            // The weakening is the `if` up to its empty block; its lines end at the block's `{`, so that a comment
            // in the block, or an `else` after it, the path that did not fail, is no line of it.
            let head: Vec<&str> = children(node)
                .into_iter()
                .take_while(|c| c.kind() != "else")
                .flat_map(|c| tokens(c, self.text))
                .collect();
            self.weakenings.push(Weakening {
                lines: Lines { first: line(node), last: line(block) },
                kind: WeakeningKind::SwallowedError,
                code: digest_of(&head),
                message: format!(
                    "`{}` added in `{}`: its empty block throws away the error that would fail the test.",
                    first_line(self.text.of(node)),
                    self.function
                ),
            });
        }
        let calls: Vec<Node> = statements
            .iter()
            .filter(|s| s.kind() == "expression_statement")
            .filter_map(|s| s.named_child(0))
            .filter(|c| c.kind() == "call_expression" && self.testing_method(*c).is_some_and(|m| FAILING.contains(&m)))
            .collect();
        if calls.is_empty() {
            return;
        }
        self.consumed.extend(calls.iter().map(|c| c.id()));
        let mut parts: Vec<Node> =
            ["initializer", "condition"].iter().filter_map(|f| node.child_by_field_name(f)).collect();
        parts.extend(&calls);
        let assertion = self.assertion(node, &parts, block.end_byte());
        self.fail(assertion);
    }

    /// The assertion that starts at `node` and ends at the byte `end`, its literals and shape those of `parts`.
    fn assertion(&self, node: Node, parts: &[Node], end: usize) -> Assertion {
        let mut literals = vec![];
        let mut shape = vec![];
        for part in parts {
            blank_literals(*part, self.text, &mut shape, &mut literals);
            shape.push(";".into());
        }
        Assertion {
            line: line(node),
            text: self.text.0[node.start_byte()..end].into(),
            shape: shape.join(" "),
            literals,
            reads: self.reads(parts),
        }
    }

    /// The names `parts`, an assertion's, read as the file's, each once, in the order written ([`read`]).
    fn reads(&self, parts: &[Node]) -> Vec<String> {
        let field = |n: Node| {
            let element = n.parent().filter(|p| p.kind() == "literal_element");
            let keyed = element.and_then(|e| e.parent()).filter(|k| k.kind() == "keyed_element");
            let key = keyed.and_then(|k| k.child_by_field_name("key")) == element && element.is_some();
            // A map's key is an expression; a literal whose type is elided, nested in another, is taken for a struct.
            let literal = keyed.and_then(|k| k.parent()).and_then(|body| body.parent());
            let of_map = literal.and_then(|l| l.child_by_field_name("type")).is_some_and(|ty| ty.kind() == "map_type");
            key && !of_map
        };
        let mut out: Vec<String> = vec![];
        // Nodes still to read, the next one last.
        let mut pending: Vec<Node> = parts.iter().rev().copied().collect();
        while let Some(node) = pending.pop() {
            let name = self.text.of(node);
            let fresh = !self.own.contains(name) && !out.iter().any(|n| n == name);
            if node.kind() == "identifier" && !field(node) && fresh {
                out.push(name.into());
            }
            pending.extend(named(node).into_iter().rev());
        }
        out
    }

    /// Whether `condition` is `err != nil` or `nil != err`, `err` naming an error ([`names_an_error`]).
    fn error_not_nil(&self, condition: Node) -> bool {
        let operator = condition.child_by_field_name("operator").map(|o| self.text.of(o));
        let (Some(left), Some(right)) = (condition.child_by_field_name("left"), condition.child_by_field_name("right"))
        else {
            return false;
        };
        let error = |n: Node| n.kind() == "identifier" && names_an_error(self.text.of(n));
        condition.kind() == "binary_expression"
            && operator == Some("!=")
            && ((error(left) && right.kind() == "nil") || (left.kind() == "nil" && error(right)))
    }

    /// An assignment, `node`: a swallowed error when it is `_ = err`.
    fn discard(&mut self, node: Node<'t>) {
        let sides = |field: &str| node.child_by_field_name(field).map(named).unwrap_or_default();
        let (left, right) = (sides("left"), sides("right"));
        let operator = children(node).into_iter().find(|c| !c.is_named()).map(|o| self.text.of(o));
        if let ([blank], [thrown], Some("=")) = (left.as_slice(), right.as_slice(), operator) {
            if self.text.of(*blank) == "_" && thrown.kind() == "identifier" && names_an_error(self.text.of(*thrown)) {
                self.weakenings.push(Weakening {
                    lines: Lines { first: line(node), last: end_line(node) },
                    kind: WeakeningKind::SwallowedError,
                    code: digest(node, self.text),
                    message: format!(
                        "`{}` added in `{}`: it throws away the error that would fail the test.",
                        first_line(self.text.of(node)),
                        self.function
                    ),
                });
            }
        }
    }
}

/// The constants of `root`, a Go file's tree, in the order written: each name of a top-level `const` given a value of
/// its own that calls nothing, as `const want = 3`; a name `_`, and one an `iota` run gives its value implicitly, as
/// `B` in `const (A = iota; B)`, are left out. The go tool refuses a second top-level declaration of a name in a
/// package, so each is bound once.
pub fn constants(root: Node, text: Text) -> Vec<Constant> {
    let declarations = named(root).into_iter().filter(|d| d.kind() == "const_declaration");
    let specs = declarations.flat_map(named).filter(|s| s.kind() == "const_spec");
    let mut out = vec![];
    for spec in specs {
        for (name, initializer) in fields(spec, "name").into_iter().zip(initializers(spec)) {
            if text.of(name) != "_" && !holds_call(initializer) {
                let expression = digest(initializer, text);
                out.push(Constant { name: text.of(name).into(), line: line(spec), expression });
            }
        }
    }
    out
}

/// The output comment of the example `node` declares, as an assertion, where `go test` runs it: a function
/// `ExampleX` with no parameters and no results, X empty or not starting with a lowercase letter, whose body's last
/// comment group, its comments on lines with none between, starts with `Output:` or `Unordered output:`, either in any
/// case, after the comment's `//` or `/*` and spaces (<https://pkg.go.dev/testing#hdr-Examples>). The assertion's text
/// is the group as written; its shape which of the two it starts with, in lowercase; and its one literal the output:
/// the text after that start, each line without its comment's `//` and the one space after, as go/doc reads a comment,
/// and with the space around the whole taken off, as `go test` compares it. None for any other node.
fn example_output(node: Node, text: Text) -> Option<Assertion> {
    let name = text.of(node.child_by_field_name("name")?);
    let suffix = name.strip_prefix("Example")?;
    let params = node.child_by_field_name("parameters").map(named).unwrap_or_default();
    if node.kind() != "function_declaration"
        || suffix.starts_with(char::is_lowercase)
        || params.iter().any(|p| p.kind() != "comment")
        || node.child_by_field_name("result").is_some()
        || node.child_by_field_name("type_parameters").is_some()
    {
        return None;
    }
    let mut comments: Vec<Node> = vec![];
    let mut pending = vec![node.child_by_field_name("body")?];
    while let Some(n) = pending.pop() {
        if n.kind() == "comment" {
            comments.push(n);
        }
        pending.extend(named(n));
    }
    comments.sort_by_key(Node::start_byte);
    let last = comments.last()?;
    let before_last = comments.windows(2).rev().take_while(|w| line(w[1]) == end_line(w[0]) + 1).count();
    let group = &comments[comments.len() - 1 - before_last..];
    let uncommented = |c: &Node| {
        let written = text.of(*c);
        match written.strip_prefix("//") {
            Some(rest) => rest.strip_prefix(' ').unwrap_or(rest),
            None => written.trim_start_matches("/*").trim_end_matches("*/"),
        }
    };
    let joined = group.iter().map(uncommented).collect::<Vec<_>>().join("\n");
    let start = joined.trim_start();
    let (shape, output) = ["unordered output:", "output:"].into_iter().find_map(|shape| {
        let head = start.get(..shape.len()).filter(|h| h.eq_ignore_ascii_case(shape))?;
        Some((shape, &start[head.len()..]))
    })?;
    Some(Assertion {
        line: line(group[0]),
        text: text.0[group[0].start_byte()..last.end_byte()].into(),
        shape: shape.into(),
        literals: vec![output.trim().into()],
        reads: vec![],
    })
}

/// Whether `element`, of a composite literal's body, is a case of a table: written in braces of its own, its value's
/// when it is keyed.
fn is_case(element: Node) -> bool {
    let written = match element.kind() {
        "literal_element" => Some(element),
        "keyed_element" => element.child_by_field_name("value"),
        _ => None,
    };
    written.and_then(|w| w.named_child(0)).is_some_and(|v| v.kind() == "literal_value")
}

/// Each name a `:=` or a `var` anywhere in `body`, a function's, binds to an expression, whatever the scope, with
/// that expression ([`initializers`]); a name declared with no value, or given one by `=`, is not one.
fn named_initializers<'t>(body: Node<'t>, text: Text<'t>) -> Vec<(&'t str, Node<'t>)> {
    let mut out = vec![];
    let mut pending = vec![body];
    while let Some(node) = pending.pop() {
        match node.kind() {
            "short_var_declaration" => {
                let names = node.child_by_field_name("left").map(named).unwrap_or_default();
                out.extend(names.into_iter().map(|n| text.of(n)).zip(initializers(node)));
            }
            "var_spec" => out.extend(fields(node, "name").into_iter().map(|n| text.of(n)).zip(initializers(node))),
            _ => {}
        }
        pending.extend(named(node));
    }
    out
}

/// The composite literals that a `for … range` in `body`, a function's, ranges over, written in the `range` or bound
/// to the name it ranges over as `bound`, `body`'s [`named_initializers`], says: its tables of cases, by their nodes'
/// ids.
fn ranged_literals(body: Node, bound: &[(&str, Node)], text: Text) -> HashSet<usize> {
    let mut ranged = vec![];
    let mut pending = vec![body];
    while let Some(node) = pending.pop() {
        if node.kind() == "range_clause" {
            ranged.extend(node.child_by_field_name("right"));
        }
        pending.extend(named(node));
    }
    let literals = ranged.into_iter().flat_map(|r| match r.kind() {
        "identifier" => {
            let of_name = bound.iter().filter(|(name, _)| *name == text.of(r));
            of_name.map(|(_, initializer)| *initializer).collect()
        }
        _ => vec![r],
    });
    literals.filter(|l| l.kind() == "composite_literal").map(|l| l.id()).collect()
}

/// The expressions a `const_spec`, `var_spec` or `short_var_declaration`, `node`, gives its names, in order, its
/// comments left out.
fn initializers(node: Node) -> Vec<Node> {
    let field = if node.kind() == "short_var_declaration" { "right" } else { "value" };
    let written = node.child_by_field_name(field).map(named).unwrap_or_default();
    written.into_iter().filter(|v| v.kind() != "comment").collect()
}

/// Whether `node` is or holds a call.
fn holds_call(node: Node) -> bool {
    let mut pending = vec![node];
    while let Some(node) = pending.pop() {
        if node.kind() == "call_expression" {
            return true;
        }
        pending.extend(named(node));
    }
    false
}

/// Whether `name` names an error by Go's habit: `err`, `err` then a capital or a digit, as `errClose` or `err2`, or a
/// name ending in `Err`, as `closeErr`.
fn names_an_error(name: &str) -> bool {
    let after = name.strip_prefix("err").map(|rest| rest.chars().next());
    matches!(after, Some(None))
        || after.flatten().is_some_and(|c| c.is_uppercase() || c.is_ascii_digit())
        || name.ends_with("Err")
}

/// Appends the tokens of `node` to `shape`, each literal as `$lit`, and each literal's text to `literals`.
fn blank_literals(node: Node, text: Text, shape: &mut Vec<String>, literals: &mut Vec<String>) {
    if node.kind() == "comment" {
        return;
    }
    if is_literal(node) {
        literals.push(text.of(node).into());
        shape.push("$lit".into());
    } else if node.child_count() == 0 {
        shape.extend(tokens(node, text).into_iter().map(String::from));
    } else {
        for child in children(node) {
            blank_literals(child, text, shape, literals);
        }
    }
}
