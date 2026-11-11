//! The cognitive-complexity check on Python read by lang_python: the score of each construct, after G. Ann Campbell's
//! paper in its mapping to Python, each expected score added up in the comments, one increment per line; then the
//! check over a change, with how it pairs a function with its base. What the gate decides over a real change before
//! the check judges it is crates/gate/tests/parser_checks.rs's.

mod common;

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::path::Path;

use checks::complexity::{scores, too_complex, LIMIT};
use checks::ChangedFile;
use common::{added, among_tests, changed, deleted, edited, replaced};

/// Each function of `source`, Python, that the check judges as one, by its qualified name, with its score.
fn scores_of(source: &str) -> BTreeMap<String, u32> {
    let file = lang_python::read(source.as_bytes(), Path::new("calc.py"), false).expect("the test's Python parses");
    file.functions.iter().zip(scores(&file)).map(|(f, n)| (f.name.clone(), n)).collect()
}

/// The score of the first function of `source`.
fn score(source: &str) -> u32 {
    let file = lang_python::read(source.as_bytes(), Path::new("calc.py"), false).expect("the test's Python parses");
    scores(&file)[0]
}

/// The score of the first method of the class `class` of `source`.
fn first_method_score(source: &str, class: &str) -> u32 {
    let file = lang_python::read(source.as_bytes(), Path::new("calc.py"), false).expect("the test's Python parses");
    let first = file.functions.iter().position(|f| f.name.starts_with(&format!("{class}."))).expect("a method");
    scores(&file)[first]
}

/// `pairs` as [`scores_of`] gives them.
fn named(pairs: &[(&str, u32)]) -> BTreeMap<String, u32> {
    pairs.iter().map(|&(name, n)| (name.to_string(), n)).collect()
}

#[test]
fn loops_and_ifs_add_one_and_their_nesting() {
    assert_eq!(score(r#"
def sum_of_primes(top):
    total = 0
    for i in range(1, top + 1):  # +1
        for j in range(2, i):    # +2
            if i % j == 0:       # +3
                break
        else:                    # +1
            total += i
    return total
"#), 7);
}

#[test]
fn elif_and_else_add_one_and_nest_their_bodies() {
    assert_eq!(score(r#"
def f(a):
    if a == 1:      # +1
        return 1
    elif a == 2:    # +1
        if a:       # +2
            return 2
    else:           # +1
        if a:       # +2
            return 3
    return 0
"#), 7);
}

#[test]
fn a_while_and_its_else() {
    assert_eq!(score(r#"
def f(n):
    while n:      # +1
        n -= 1
    else:         # +1
        n = 0
    return n
"#), 2);
}

#[test]
fn match_adds_one_for_all_its_cases_and_nests_them() {
    assert_eq!(score(r#"
def get_words(n):
    match n:              # +1
        case 1:
            return "one"
        case 2 if n > 0:  # a guard adds nothing of its own
            return "a couple"
        case _:
            if n:         # +2
                return "lots"
"#), 3);
}

#[test]
fn except_adds_one_and_its_nesting_while_try_finally_and_with_add_nothing() {
    assert_eq!(score(r#"
def f(path):
    try:
        with open(path) as f:  # neither try nor with nests
            if f:              # +1
                pass
    except OSError:            # +1
        if path:               # +2
            pass
    except ValueError:         # +1
        pass
    else:                      # +1
        pass
    finally:
        if path:               # +1
            pass
"#), 7);
}

#[test]
fn each_sequence_of_like_boolean_operators_adds_one() {
    assert_eq!(score(r#"
def f(a, b, c, d):
    if a and b and c:       # +1 if, +1 and
        pass
    if a and b or c and d:  # +1 if, +1 and, +1 or, +1 and
        pass
    x = a or b and c or d   # +1 or, +1 and, +1 or
    y = a or not (b or c)   # +1 or, +1 or: not starts a sequence of its own
    z = (1 if a else 0) or b and c  # +1 or, +1 and, +1 ternary
    return a and (b and c)  # +1: the parentheses do not show in the syntax tree
"#), 15);
}

#[test]
fn except_and_match_take_their_nesting_too() {
    assert_eq!(score(r#"
def f(xs):
    for x in xs:            # +1
        try:
            pass
        except ValueError:  # +2
            pass
        match x:            # +2
            case _:
                pass
"#), 5);
}

#[test]
fn a_ternary_adds_one_and_its_nesting_and_nests_what_it_holds() {
    assert_eq!(score(r#"
def f(a, b):
    for x in a:                          # +1
        y = 1 if x else (2 if b else 3)  # +2, +3
    return y
"#), 6);
}

#[test]
fn what_a_statement_reads_before_its_body_counts_at_the_statement_s_level() {
    assert_eq!(score(r#"
def f(xs, key=lambda x: 1 if x else 0):  # the judged function's own defaults count for nothing
    for x in (xs if xs else []):         # +1 for, +1 ternary
        while (x if x else 0):           # +2 while, +2 ternary
            if (x if x else 0):          # +3 if, +3 ternary
                x -= 1
            elif x:                      # +1
                x += 1
    match (xs if xs else None):          # +1 match, +1 ternary
        case _:
            pass
    try:
        pass
    except (ValueError if xs else TypeError):  # +1 except, +1 ternary
        pass
    def g(y=(1 if xs else 2)):           # +1 ternary
        return y
    h = lambda y=(1 if xs else 2): y     # +1 ternary
    return g, h
"#), 19);
}

#[test]
fn comprehensions_add_nothing_and_lambdas_and_nested_functions_nest() {
    assert_eq!(score(r#"
def f(xs):
    ys = [x for x in xs if x for y in x if not y]
    key = lambda x: 1 if x else 2          # +2
    def inner(x):
        if x:                              # +2
            return [y for y in x if y and x]  # +1
        return inner(x - 1)                # +1, recursion
    class Box:
        def size(self):
            return 3 if self else 4        # +2
    return sorted(ys, key=key), inner, Box
"#), 8);
}

#[test]
fn a_function_that_calls_itself_adds_one() {
    assert_eq!(score(r#"
def walk(node):
    if node is None:                      # +1
        return 0
    return walk(node.left) + walk(node.right)  # +1, once
"#), 2);
}

#[test]
fn a_method_calls_itself_through_self_cls_or_its_class() {
    let source = r#"
class Tree:
    def depth(self, node):
        return max(self.depth(c) for c in node)  # +1
    def size(self, node):
        return Tree.size(self, node)            # +1
    def leaves(self, node):
        return leaves(node)                     # a module-level function, not this method
    @classmethod
    def build(cls, n):
        return cls.build(n - 1)                 # +1
"#;
    assert_eq!(first_method_score(source, "Tree"), 1);
    assert_eq!(scores_of(source), named(&[("Tree.depth", 1), ("Tree.size", 1), ("Tree.leaves", 0), ("Tree.build", 1)]));
}

#[test]
fn each_function_in_a_cycle_of_two_scores_the_recursion_point() {
    assert_eq!(scores_of(r#"
def even(n):
    return n == 0 or odd(n - 1)   # +1 or, +1 cycle
def odd(n):
    return n != 0 and even(n - 1)  # +1 and, +1 cycle
def unrelated(n):
    return even(n)
"#), named(&[("even", 2), ("odd", 2), ("unrelated", 0)]));
}

#[test]
fn each_function_in_a_cycle_of_three_scores_the_recursion_point_and_one_leading_into_it_none() {
    assert_eq!(scores_of(r#"
def a(x):
    return b(x)
def b(x):
    return c(x)
def c(x):
    return a(x)
def enter(x):
    return a(x)
"#), named(&[("a", 1), ("b", 1), ("c", 1), ("enter", 0)]));
}

#[test]
fn methods_in_a_cycle_through_self_score_the_recursion_point() {
    assert_eq!(scores_of(r#"
class Parser:
    def x(self, s):
        return self.y(s)
    def y(self, s):
        return Parser.x(self, s)
    def z(self, s):
        return self.x(s)
"#), named(&[("Parser.x", 1), ("Parser.y", 1), ("Parser.z", 0)]));
}

#[test]
fn a_cycle_through_an_alias_is_not_found() {
    assert_eq!(scores_of(r#"
def a(x):
    f = b
    return f(x)
def b(x):
    return a(x)
"#), named(&[("a", 0), ("b", 0)]));
}

#[test]
fn a_call_to_a_name_the_function_binds_is_not_a_call_to_the_module_s_function() {
    assert_eq!(scores_of(r#"
def a(b):
    return b()            # the parameter
def b():
    return a(len)
def c():
    def d():
        return 1
    return d()            # the nested function
def d():
    return c()
def e(items):
    f = sorted
    return f(items)       # the local
def f(items):
    return e(items)
"#), named(&[("a", 0), ("b", 0), ("c", 0), ("d", 0), ("e", 0), ("f", 0)]));
}

#[test]
fn every_way_of_binding_a_name_shadows_the_module_s_function_but_global() {
    assert_eq!(scores_of(r#"
def a(x):
    import b
    from c import d as e
    try:
        pass
    except ValueError as f:
        pass
    match x:
        case [*g]:
            pass
        case {**h}:
            pass
        case i:
            pass
    return b() + e() + f() + g() + h() + i()
def b(): return a(0)
def e(): return a(0)
def f(): return a(0)
def g(): return a(0)
def h(): return a(0)
def i(): return a(0)
def j():
    global k
    k = wrap(k)
    return k()            # the module's k, which it has just rebound
def k(): return j()
"#), named(&[("a", 2), ("b", 0), ("e", 0), ("f", 0), ("g", 0), ("h", 0), ("i", 0), ("j", 1), ("k", 1)]));
}

#[test]
fn a_name_bound_only_in_a_nested_scope_leaves_the_function_s_own_call_to_the_module() {
    assert_eq!(scores_of(r#"
def f(x):
    items = [f for f in x]     # the comprehension's own f
    return f(x[1:])            # +1, recursion
def walk(node):
    key = lambda walk: walk.name
    return walk(node.child)    # +1, recursion
def g(x):
    def inner():
        h = 1
        return h
    return h(x)                # +1, the cycle with h
def h(x):
    return g(x)
"#), named(&[("f", 1), ("walk", 1), ("g", 1), ("h", 1)]));
}

#[test]
fn a_type_parameter_is_not_the_module_s_function() {
    assert_eq!(scores_of(r#"
def T():
    return build(0)
def build[T](x):
    return T(x)
class Box[U]:
    def get(self):
        return U()            # the class's type parameter
def U():
    return Box.get(None)
"#), named(&[("T", 0), ("build", 0), ("Box.get", 0), ("U", 0)]));
}

#[test]
fn a_type_parameter_list_in_a_class_body_sees_the_class_s_names() {
    assert_eq!(scores_of(r#"
def f():
    class C:
        def f(): return 1
        def m[T: f()](self): pass     # C's f, as PEP 695 reads a bound
    return C
def g():
    class C:
        g = object
        class D[T](g()): pass         # C's g, as PEP 695 reads a base
    return C
def h():
    class C:
        def h(): return 1
        def m[T](self):
            return h()                # the module's h: a method's body does not see C's names
    return C
"#), named(&[("f", 0), ("g", 0), ("h", 1)]));
}

#[test]
fn an_assignment_expression_in_a_comprehension_binds_in_the_function() {
    assert_eq!(scores_of(r#"
def f(handlers, x):
    if any((g := h) for h in handlers):   # +1 if
        return g(x)                       # the g the walrus bound
def g(x):
    return f([], x)
"#), named(&[("f", 1), ("g", 0)]));
}

#[test]
fn what_python_reads_outside_a_nested_scope_is_read_in_the_function() {
    assert_eq!(scores_of(r#"
def a(x):
    return [a for a in a(x[1:])]          # +1, the first iterable is read in a's scope
def b(x, key=lambda b: b):
    return b(x[1:])                       # +1, the lambda's b is the lambda's alone
def c(x):
    def inner(y: c(x) = 0):               # +1, an annotation is read in c's scope
        pass
    return inner
def d():
    class C[T: d()]:                      # +1, a bound is read in the type parameters' scope
        pass
    return C
def e():
    @e()                                  # +1, a decorator is read outside the type parameters
    class C[e]:
        pass
    return C
"#), named(&[("a", 1), ("b", 1), ("c", 1), ("d", 1), ("e", 1)]));
}

#[test]
fn a_global_or_nonlocal_in_a_nested_scope_leaves_the_outer_binding_alone() {
    assert_eq!(scores_of(r#"
def f():
    g = make()
    def reset():
        nonlocal g
        g = make()
    return g()
def g():
    return f()
def k():
    return m()
def m():
    global k
    def inner(k):
        return k()
    return inner
"#), named(&[("f", 0), ("g", 0), ("k", 0), ("m", 0)]));
}

#[test]
fn self_in_a_class_nested_in_a_method_is_that_class_s() {
    assert_eq!(scores_of(r#"
class C:
    def x(self):
        class D:
            def z(self):
                return self.y()
        return D
    def y(self):
        return self.x()
"#), named(&[("C.x", 0), ("C.y", 0)]));
}

#[test]
fn methods_of_two_nested_classes_of_one_name_are_told_apart() {
    assert_eq!(scores_of(r#"
class A:
    class Meta:
        def x(self):
            return self.y()
class B:
    class Meta:
        def y(self):
            return self.x()
"#), named(&[("A.Meta.x", 0), ("B.Meta.y", 0)]));
}

#[test]
fn a_function_that_calls_itself_and_lies_on_a_cycle_scores_the_point_once() {
    assert_eq!(scores_of(r#"
def walk(node):
    return walk(node.left) + visit(node)
def visit(node):
    return walk(node.right)
def alone(n):
    return alone(n - 1)
"#), named(&[("walk", 1), ("visit", 1), ("alone", 1)]));
}

#[test]
fn a_method_of_a_class_nested_in_the_function_recurses_as_a_method_does() {
    assert_eq!(score(r#"
def outer():
    class K:
        def m(self):
            return m()       # a module-level m, not this method
    return K
"#), 0);
    assert_eq!(score(r#"
def outer():
    class K:
        def m(self):
            if self:
                return self.m()  # +1 recursion
            return K.m(self)
    return K
"#), 3);
}

/// A function of `n` ifs one after the other, so of cognitive complexity `n`, each line indented by `indent`.
fn flat(name: &str, n: u32, indent: &str) -> String {
    let ifs: String =
        (0..n).map(|k| format!("{indent}    if x == '{name} {k}':\n{indent}        x += {k}\n")).collect();
    format!("{indent}def {name}(x):\n{ifs}{indent}    return x\n")
}

/// A function of two lines whose syntax tree is deeper than a recursive walk could go: 3000 operands of `operator`.
fn long_chain(name: &str, operator: &str) -> String {
    format!("def {name}(a):\n    return {}\n", vec!["a"; 3000].join(&format!(" {operator} ")))
}

/// A function as deep as [`long_chain`]'s by its branches: an if with 1000 elifs, which scores 1001.
fn long_elif() -> String {
    let elifs: String = (1..=1000).map(|k| format!("    elif x == {k}:\n        pass\n")).collect();
    format!("def long_elif(x):\n    if x == 0:\n        pass\n{elifs}")
}

#[test]
fn code_deeper_than_a_recursive_walk_could_go_is_scored() {
    assert_eq!(score(&long_chain("long_sum", "+")), 0);
    // The if, and one for each elif.
    assert_eq!(score(&long_elif()), 1001);
    assert_eq!(score(&long_chain("long_and", "and")), 1);
}

/// The body of the first function of `source`.
fn body(source: &str) -> String {
    let file = lang_python::read(source.as_bytes(), Path::new("calc.py"), false).expect("the test's Python parses");
    file.functions[0].body.clone()
}

#[test]
fn a_body_is_the_same_whatever_its_name_layout_comments_and_quotes() {
    let written = body("def f(a):\n    if a:\n        return 'x'\n    return [a,\n            1]\n");
    assert_eq!(written, body("def g(a):\n    # why\n    if a:  # when\n        return \"x\"\n\n    return [a, 1]\n"));
    assert_eq!(
        written,
        body("class C:\n    @staticmethod\n    def f(a):\n        if a:\n            return 'x'\n\
              \x20       return [a, 1]\n")
    );
}

#[test]
fn a_body_differs_with_its_structure_and_its_literals() {
    // Only the indentation tells these two apart.
    assert_ne!(
        body("def f(a):\n    if a:\n        a = 1\n    return a\n"),
        body("def f(a):\n    if a:\n        a = 1\n        return a\n")
    );
    assert_ne!(body("def f(a, b, c):\n    return {**a, b: c}\n"), body("def f(a, b, c):\n    return {b: a, **c}\n"));
    assert_ne!(
        body("def f():\n    def g(*, a=1, b):\n        pass\n"),
        body("def f():\n    def g(*, a, b=1):\n        pass\n")
    );
    assert_ne!(body("def f():\n    return 'x'\n"), body("def f():\n    return b'x'\n"));
    // One name unpacked from a sequence, and the sequence itself: tree-sitter gives both targets one kind.
    assert_ne!(body("def f(y):\n    (x,) = y()\n    return x\n"), body("def f(y):\n    (x) = y()\n    return x\n"));
    assert_ne!(
        body("def f(rows):\n    for (name,) in rows:\n        print(name)\n"),
        body("def f(rows):\n    for (name) in rows:\n        print(name)\n")
    );
    assert_eq!(body("def f(y):\n    (x, z,) = y()\n"), body("def f(y):\n    (x, z) = y()\n"));
    assert_ne!(body("def f():\n    return 'x'\n"), body("def f():\n    return x\n"));
    let huge = format!("def f():\n    return 0x{}\n", "f".repeat(4000));
    assert_ne!(body(&huge), body(&huge.replacen("0xf", "0xe", 1)));
}

#[test]
fn the_seven_functions_sonarqube_flagged_score_over_the_limit() {
    let source = include_str!("data/s3776_at_84e87e7.py.txt");
    let scores = scores_of(source);
    let names: Vec<&str> = scores.keys().map(|name| name.rsplit('.').next().unwrap()).collect();
    let mut expected = ["main", "_turn", "_poll", "quality_report", "changed_lines", "_shared_name", "_parse_step"];
    expected.sort_unstable();
    let mut found = names.clone();
    found.sort_unstable();
    assert_eq!(found, expected);
    for (name, n) in &scores {
        assert!(*n > LIMIT, "{name}: {n}");
    }
}

/// A finding as these tests compare it: its path, line and rule.
type Found = (String, Option<u32>, String);

/// The findings of the check on `files`, by gate.toml's default `complexity.exempt`: the files under a `tests`
/// directory.
fn found(files: &[ChangedFile]) -> (Vec<Found>, Vec<String>) {
    let exempt = |path: &OsStr| path.to_str().is_some_and(|p| p.split('/').any(|part| part == "tests"));
    let found = too_complex(files, &exempt);
    let place = |f: &checks::Finding| {
        (f.path.as_ref().unwrap().to_string_lossy().into_owned(), f.line, f.rule.clone())
    };
    (found.iter().map(place).collect(), found.into_iter().map(|f| f.message).collect())
}

fn over_limit(path: &str, line: u32) -> Found {
    (path.into(), Some(line), "cognitive-complexity over-limit".into())
}

const CALC: &str = "calc.py";

/// calc.py at the base of the changes below: grow just under the limit, big over it and small well under it.
fn base() -> String {
    format!("{}\n\n{}\n\n{}", flat("grow", 14, ""), flat("big", 20, ""), flat("small", 3, ""))
}

/// [`base`]'s big under the name `name`, its body as it is there.
fn big_as(name: &str) -> String {
    replaced(&flat("big", 20, ""), "def big(", &format!("def {name}("))
}

/// [`base`] once big is moved out of it.
fn without_big() -> String {
    replaced(&base(), &format!("\n\n{}", big_as("big")), "")
}

/// [`base`] with an edit inside big that leaves it at 20.
fn big_edited() -> String {
    replaced(&base(), "'big 3':\n        x += 3\n", "'big 3':\n        x += 30\n")
}

/// [`base`] with big renamed in place, its body unchanged.
fn big_renamed() -> String {
    replaced(&base(), "def big(", "def legacy_big(")
}

/// (path, message) of each of the check's findings on `files` and on fresh.py, which holds a new function over the
/// limit, but the finding on fresh.py: so a test that expects no finding fails when the check finds nothing at all.
fn found_beside_fresh(mut files: Vec<ChangedFile>) -> Vec<(String, String)> {
    files.push(added("fresh.py", &flat("fresh", 16, "")));
    let (places, messages) = found(&files);
    let mut beside: Vec<(String, String)> = places.into_iter().map(|(path, ..)| path).zip(messages).collect();
    let fresh = beside.iter().position(|(path, _)| path == "fresh.py").expect("fresh.py's function is found");
    beside.remove(fresh);
    beside
}

/// What [`found_beside_fresh`] gives for the function `name` of the file at `path` that scores `score` with no base.
fn new_function(path: &str, name: &str, score: u32) -> (String, String) {
    (path.into(), format!("`{name}`: cognitive complexity {score}, a new function; the limit is 15."))
}

#[test]
fn a_new_function_over_the_limit_is_found_and_one_at_it_is_not() {
    let source = format!("{}\n\n{}", flat("new_ok", 15, ""), flat("new_big", 16, ""));
    let (place, messages) = found(&[added("calc.py", &source)]);
    assert_eq!(place, [over_limit("calc.py", 35)]);
    assert_eq!(messages, ["`new_big`: cognitive complexity 16, a new function; the limit is 15."]);
}

#[test]
fn nesting_added_inside_a_function_is_found_at_its_def_line_with_its_base_s_score() {
    let base = format!("@staticmethod\n{}", flat("grow", 14, ""));
    let now = base.replace("        x += 13\n", "        x += 13\n        if x:\n            x -= 1\n");
    let (place, messages) = found(&[edited("calc.py", &base, &now)]);
    assert_eq!(place, [over_limit("calc.py", 2)]);
    assert_eq!(messages, ["`grow`: cognitive complexity 16, 14 at the base; the limit is 15."]);
}

#[test]
fn a_method_is_named_and_scored_with_its_class() {
    // 15 ifs, and a call of itself through self: 16.
    let method = flat("size", 15, "    ").replace("return x", "return self.size(x)");
    let (place, messages) = found(&[added("shapes.py", &format!("class Box:\n{method}"))]);
    assert_eq!(place, [over_limit("shapes.py", 2)]);
    assert!(messages[0].starts_with("`Box.size`: cognitive complexity 16, a new function"), "{messages:?}");
}

#[test]
fn a_function_whose_decorator_the_change_adds_is_compared_with_its_unchanged_self() {
    let base = flat("big", 20, "");
    let now = format!("@staticmethod\n{base}");
    assert_eq!(found(&[edited("calc.py", &base, &now)]).0, []);
    // A comment added after its last statement is no edit of it.
    let after = format!("{base}# what follows\n\n\ndef next(x):\n    return x\n");
    let before = format!("{base}\n\ndef next(x):\n    return x\n");
    assert_eq!(found(&[edited("calc.py", &before, &after)]).0, []);
}

#[test]
fn a_name_defined_twice_is_compared_with_the_same_definition_at_the_base() {
    // A property's getter, scoring 21, then its setter, which scores nothing, under the same name.
    let getter = flat("x", 21, "    ").replace("    def x(x):\n", "    @property\n    def x(self):\n");
    let base = format!("class C:\n{getter}\n    @x.setter\n    def x(self, value):\n        pass\n");
    assert_eq!(found(&[edited("props.py", &base, &base.replace("x += 3\n", "x += 30\n"))]).0, []);
    let worse = base.replace("x += 3\n", "x += 3\n        if x:\n            x -= 1\n");
    let (_, messages) = found(&[edited("props.py", &base, &worse)]);
    assert_eq!(messages, ["`C.x`: cognitive complexity 22, 21 at the base; the limit is 15."]);
}

#[test]
fn methods_of_nested_classes_and_functions_under_module_level_statements_are_judged() {
    let source = format!(
        "class Outer:\n    class Inner:\n{}if True:\n{}try:\n    import json\nexcept ImportError:\n{}",
        flat("method", 16, "        "),
        flat("under_if", 16, "    "),
        flat("under_except", 16, "    ")
    );
    let (_, messages) = found(&[added("more.py", &source)]);
    let names: Vec<&str> = messages.iter().map(|m| m.split(':').next().unwrap()).collect();
    assert_eq!(names, ["`Outer.Inner.method`", "`under_if`", "`under_except`"]);
}

#[test]
fn only_the_files_the_settings_exempt_are_not_judged() {
    let files: Vec<ChangedFile> = ["tests/test_calc.py", "tests/unit/helpers.py", "test_top.py", "src/test_utils.py"]
        .iter()
        .map(|path| added(path, &flat("helper", 16, "")))
        .collect();
    assert!(among_tests("test_top.py") && among_tests("src/test_utils.py"));
    assert_eq!(found(&files).0, [over_limit("src/test_utils.py", 1), over_limit("test_top.py", 1)]);
}

#[test]
fn files_that_cannot_be_read_or_parsed_are_findings_and_the_others_are_still_judged() {
    let files = [
        added("a_unknown.py", "# -*- coding: nonsense -*-\n"),
        added("b_broken.py", "x = 1\ndef broken(:\n"),
        added("calc.py", &flat("big", 16, "")),
    ];
    let (place, messages) = found(&files);
    let not_scored = |path: &str, line| (path.to_string(), line, "cognitive-complexity not-scored".to_string());
    assert_eq!(place, [not_scored("a_unknown.py", None), not_scored("b_broken.py", Some(2)), over_limit("calc.py", 1)]);
    assert!(messages[0].starts_with("could not be read ("), "{messages:?}");
    assert_eq!(messages[1], "does not parse as Python, so its functions were not scored.");
}

#[test]
fn a_base_that_does_not_parse_leaves_its_functions_judged_as_new() {
    let (_, messages) = found(&[edited("broken.py", "def big(:\n", &flat("big", 16, ""))]);
    assert_eq!(messages, ["`big`: cognitive complexity 16, a new function; the limit is 15."]);
}

#[test]
fn a_base_whose_coding_cookie_names_no_text_encoding_leaves_its_functions_judged_as_new() {
    // rot13 is a codec that decodes no bytes to text, which Python refuses; the gate reads no cookie but
    // UTF-8's, ASCII's and Latin-1's (docs/port-differences.md, "Reading a Python file").
    let rotated = flat("rotated", 20, "");
    let now = replaced(&rotated, "x += 3\n", "x += 30\n");
    let files = vec![edited("rot.py", &format!("# coding: rot13\n{rotated}"), &now)];
    assert_eq!(found_beside_fresh(files), [new_function("rot.py", "rotated", 20)]);
}

#[test]
fn code_deeper_than_a_recursive_walk_could_go_is_judged_in_the_change_and_at_the_base() {
    let deep = format!("{}\n\n{}", long_chain("long_sum", "+"), long_elif());
    assert_eq!(found(&[added("deep.py", &deep)]).0, [over_limit("deep.py", 5)]);
    // An else: 1001 becomes 1002.
    let last = "    elif x == 1000:\n        pass\n";
    let deeper = replaced(&deep, last, &format!("{last}    else:\n        pass\n"));
    let (_, messages) = found(&[edited("deep.py", &deep, &deeper)]);
    assert_eq!(messages, ["`long_elif`: cognitive complexity 1002, 1001 at the base; the limit is 15."]);
}

#[test]
fn an_integer_too_long_to_print_in_decimal_is_judged_like_any_code() {
    // 4000 hex digits are about 4800 decimal ones, past the 4300 that Python's int prints.
    let source = format!("def h():\n    return 0x{}\n\n\n{}", "f".repeat(4000), flat("big_one", 16, ""));
    assert_eq!(found(&[added("huge.py", &source)]).0, [over_limit("huge.py", 5)]);
}

#[test]
fn a_score_that_rises_to_the_limit_is_not_found() {
    // grow: 14 becomes 15.
    let now = replaced(&base(), "    if x == 'grow 13':\n", "    if x == 'grow 13' and x:\n");
    assert_eq!(found_beside_fresh(vec![edited(CALC, &base(), &now)]), []);
}

#[test]
fn a_function_over_the_limit_whose_score_the_change_leaves_alone_is_not_found() {
    assert_eq!(found_beside_fresh(vec![edited(CALC, &base(), &big_edited())]), []);
}

#[test]
fn a_method_is_scored_with_its_class_at_the_base_too() {
    // 15 ifs, and a call of itself through self: 16 at the base and after an edit that adds no point.
    let method = flat("size", 15, "    ").replace("return x", "return self.size(x)");
    let base = format!("class Box:\n{method}");
    assert_eq!(found_beside_fresh(vec![edited("shapes.py", &base, &replaced(&base, "x += 3\n", "x += 30\n"))]), []);
}

#[test]
fn an_unchanged_function_moved_and_renamed_is_compared_with_its_old_self() {
    let files = vec![edited(CALC, &base(), &without_big()), added("other.py", &big_as("huge"))];
    assert_eq!(found_beside_fresh(files), []);
}

#[test]
fn an_unchanged_function_moved_out_of_a_deleted_file_is_compared_with_its_old_self() {
    let files = vec![deleted(CALC, &base()), added("other.py", &format!("CONSTANT = 0\n\n\n{}", big_as("big")))];
    assert_eq!(found_beside_fresh(files), []);
}

#[test]
fn an_unchanged_function_made_a_method_in_its_file_is_compared_with_its_old_self() {
    let now = replaced(&base(), &big_as("big"), &format!("class Box:\n{}", flat("big", 20, "    ")));
    assert_eq!(found_beside_fresh(vec![edited(CALC, &base(), &now)]), []);
}

#[test]
fn a_copy_of_a_function_over_the_limit_is_new_wherever_it_goes() {
    let copy = big_as("copied");
    let beside = format!("{}\n\n{copy}", base());
    assert_eq!(found_beside_fresh(vec![edited(CALC, &base(), &beside)]), [new_function(CALC, "copied", 20)]);
    // In another file: the original's file untouched, then edited elsewhere.
    let new = [new_function("other.py", "copied", 20)];
    assert_eq!(found_beside_fresh(vec![added("other.py", &copy)]), new);
    let elsewhere = replaced(&base(), "'small 2':\n        x += 2\n", "'small 2':\n        x += 20\n");
    assert_eq!(found_beside_fresh(vec![edited(CALC, &base(), &elsewhere), added("other.py", &copy)]), new);
}

#[test]
fn a_function_moved_and_made_more_complex_is_judged_as_new() {
    let worse = replaced(&big_as("big"), "        x += 19\n", "        x += 19\n        if x:\n            x -= 1\n");
    let files = vec![edited(CALC, &base(), &without_big()), added("other.py", &worse)];
    assert_eq!(found_beside_fresh(files), [new_function("other.py", "big", 22)]);
}

#[test]
fn a_function_moved_unchanged_out_of_tests_is_judged_as_new() {
    // The tests were never held to the limit, so they lend no base to a function that is.
    let helper = flat("helper", 20, "");
    let files = vec![edited("tests/helpers.py", &format!("X = 0\n\n\n{helper}"), "X = 0\n"), added("lib.py", &helper)];
    assert_eq!(found_beside_fresh(files), [new_function("lib.py", "helper", 20)]);
}

#[test]
fn a_function_moved_unchanged_where_nothing_is_judged_keeps_its_base_from_a_copy_of_any_name() {
    for copy in ["copied", "big"] {
        let files = vec![
            edited(CALC, &base(), &without_big()),
            added("tests/helpers.py", &big_as("big")),
            added("lib.py", &big_as(copy)),
        ];
        assert_eq!(found_beside_fresh(files), [new_function("lib.py", copy, 20)], "{copy}");
    }
}

/// tests/helpers.py with [`base`]'s big in it on both sides, a copy of calc.py's, edited above it: a file the check
/// does not judge.
fn helpers_with_big() -> ChangedFile {
    let helpers = format!("X = 0\n\n\n{}", big_as("big"));
    edited("tests/helpers.py", &helpers, &replaced(&helpers, "X = 0\n", "X = 1\n"))
}

#[test]
fn a_function_in_a_file_the_check_does_not_judge_keeps_its_own_base_from_a_moved_twin() {
    // calc.py's big, moved unchanged to lib.py.
    let files = vec![edited(CALC, &base(), &without_big()), added("lib.py", &big_as("big")), helpers_with_big()];
    assert_eq!(found_beside_fresh(files), []);
}

#[test]
fn an_unchanged_copy_in_a_file_the_check_does_not_judge_takes_no_base_from_its_original() {
    assert_eq!(found_beside_fresh(vec![edited(CALC, &base(), &big_edited()), helpers_with_big()]), []);
    // Nor from its original renamed in place, when the change adds the copy.
    let files = vec![edited(CALC, &base(), &big_renamed()), added("tests/helpers.py", &big_as("big"))];
    assert_eq!(found_beside_fresh(files), []);
}

#[test]
fn a_copy_of_its_name_takes_no_base_from_an_original_renamed_in_place() {
    let files = vec![edited(CALC, &base(), &big_renamed()), added("lib.py", &big_as("big"))];
    assert_eq!(found_beside_fresh(files), [new_function("lib.py", "big", 20)]);
}

#[test]
fn a_function_moved_unchanged_keeps_its_base_from_a_new_function_under_its_old_name() {
    let new_big = flat("big", 18, "").replace("'big ", "'new big ");
    let calc = replaced(&base(), &big_as("big"), &new_big);
    for to in ["lib.py", "tests/helpers.py", "bin/tool"] {
        let first = if to == "bin/tool" { "#!/usr/bin/env python3\n" } else { "" };
        let files = vec![edited(CALC, &base(), &calc), added(to, &format!("{first}{}", big_as("big")))];
        assert_eq!(found_beside_fresh(files), [new_function(CALC, "big", 18)], "{to}");
    }
}

#[test]
fn a_function_renamed_unchanged_keeps_its_base_from_a_new_function_under_its_old_name() {
    let new_big = flat("big", 16, "").replace("'big ", "'new big ");
    let calc = format!("{}\n\n{new_big}", big_renamed());
    assert_eq!(found_beside_fresh(vec![edited(CALC, &base(), &calc)]), [new_function(CALC, "big", 16)]);
}

#[test]
fn a_function_moved_unchanged_out_of_a_file_that_keeps_its_name_keeps_its_base_from_a_copy() {
    // Two functions named _, as @singledispatch registrations are: the one scoring 20 moves where nothing is judged,
    // the other stays, and a copy of the first is added elsewhere.
    let moved = flat("_", 20, "").replace("'_ ", "'moved ");
    let stays = flat("_", 3, "").replace("'_ ", "'stays ");
    let files = vec![
        edited("disp.py", &format!("{moved}\n\n{stays}"), &stays),
        added("tests/helpers.py", &moved),
        added("lib.py", &moved),
    ];
    assert_eq!(found_beside_fresh(files), [new_function("lib.py", "_", 20)]);
}

#[test]
fn a_function_moved_unchanged_out_of_a_file_that_keeps_its_twin_keeps_its_base() {
    let twin = big_as("twin");
    let with_twin = format!("{}\n\n{twin}", base());
    assert_eq!(found_beside_fresh(vec![edited(CALC, &with_twin, &base()), added("lib.py", &twin)]), []);
}

#[test]
fn a_copy_of_a_function_the_change_edits_takes_its_base_as_bodies_first_decides() {
    // The cost TODO.md records ("A copy of a function whose original the change edits keeps the old score and
    // passes"): the copy is taken for the original moved unchanged, and the edited original for a new function.
    for copy in ["lib.py", "tests/helpers.py"] {
        let files = vec![edited(CALC, &base(), &big_edited()), added(copy, &big_as("big"))];
        assert_eq!(found_beside_fresh(files), [new_function(CALC, "big", 20)], "{copy}");
    }
}

#[test]
fn one_base_function_is_the_base_of_one_function_only() {
    let moved = [edited(CALC, &base(), &without_big()), added("other.py", &big_as("big"))];
    let copy = |path: &str| added(path, &big_as("copied"));
    let copy_last = [&moved[..], &[copy("third.py")]].concat();
    assert_eq!(found_beside_fresh(copy_last), [new_function("third.py", "copied", 20)]);
    // The copy in a file read before the original's new one, so that only the name decides.
    let copy_first = [&[copy("a_copy.py")], &moved[..]].concat();
    assert_eq!(found_beside_fresh(copy_first), [new_function("a_copy.py", "copied", 20)]);
}

/// A function named _ that does nothing, as one @singledispatch registration or @overload stub can be.
const TRIVIAL: &str = "def _(a):\n    return a\n\n\n";

#[test]
fn a_namesake_removed_or_added_above_a_function_leaves_it_compared_with_its_own_base() {
    let big = flat("_", 20, "");
    let still_20 = replaced(&big, "x += 3\n", "x += 30\n");
    let namesake_removed = edited("disp.py", &format!("{TRIVIAL}{big}"), &still_20);
    assert_eq!(found_beside_fresh(vec![namesake_removed]), []);
    let namesake_added = edited("disp.py", &big, &format!("{TRIVIAL}{still_20}"));
    assert_eq!(found_beside_fresh(vec![namesake_added]), []);
}

#[test]
fn a_rise_of_a_function_whose_name_others_share_is_found() {
    let big = flat("_", 20, "");
    let worse = replaced(&big, "        x += 19\n", "        x += 19\n        if x:\n            x -= 1\n");
    let (_, messages) = found(&[edited("disp.py", &big, &worse)]);
    assert_eq!(messages, ["`_`: cognitive complexity 22, 20 at the base; the limit is 15."]);
}

#[test]
fn a_file_renamed_and_edited_is_compared_with_its_base_under_its_old_name() {
    let renamed = changed(Some((CALC, &base())), Some(("lib.py", &big_edited())));
    assert_eq!(found_beside_fresh(vec![renamed]), []);
}
