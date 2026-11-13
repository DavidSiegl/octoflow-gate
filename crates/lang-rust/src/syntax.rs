//! The walk of a Rust file's tree-sitter tree that gathers what the model is made of: each `fn` with the items that
//! hold it, its attributes, assertions and discarded values and, for one no other `fn` holds, the flow of its body;
//! each inline module; and every name the file binds.
//!
//! A macro's arguments are walked as code when they are expressions separated by commas, as an assertion's or
//! `vec![...]`'s are; any other macro's tokens are a language of its own, left unread, as clippy's own
//! `cognitive_complexity` reads no macro. tree-sitter leaves them a token tree, so [`Walk::macro_args`] parses each
//! argument again on its own.

use model::{Assertion, Binding, Flow};
use tree_sitter::{Node, Parser, Tree};

/// The macros a test asserts with, by the last segment of their path.
const ASSERTION_MACROS: &[&str] =
    &["assert", "assert_eq", "assert_ne", "debug_assert", "debug_assert_eq", "debug_assert_ne", "panic"];

/// What a macro argument is wrapped in to parse it alone as an expression: the argument is the one expression in the
/// parentheses, and the newline ends a line comment it may end with.
const ARG_PREFIX: &[u8] = b"fn f(){(";
const ARG_SUFFIX: &[u8] = b"\n);}";

/// The text a tree was parsed from, the file's own or a macro argument's wrapped (`ARG_PREFIX`), and where its bytes
/// lie in the file.
#[derive(Clone, Copy)]
pub struct Text<'t> {
    pub bytes: &'t [u8],
    /// What to add to a byte offset in `bytes` for the offset in the file of the same byte.
    shift: isize,
}

impl<'t> Text<'t> {
    /// The text of a whole file, `bytes`, whose offsets are the file's.
    pub fn of_file(bytes: &'t [u8]) -> Text<'t> {
        Text { bytes, shift: 0 }
    }

    /// The offset in the file of where `node` starts.
    pub fn start(&self, node: Node) -> usize {
        node.start_byte().checked_add_signed(self.shift).unwrap_or(0)
    }

    /// The offset in the file just past where `node` ends: exclusive, so its last byte is at one less.
    pub fn end(&self, node: Node) -> usize {
        node.end_byte().checked_add_signed(self.shift).unwrap_or(0)
    }

    /// `node`'s source text, as written.
    pub fn of(&self, node: Node) -> &'t str {
        std::str::from_utf8(&self.bytes[node.byte_range()]).unwrap_or_default()
    }
}

/// Line numbers of a file's byte offsets.
pub struct Lines {
    /// The offset of the first byte of each line.
    starts: Vec<usize>,
}

impl Lines {
    pub fn of(bytes: &[u8]) -> Lines {
        let ends = bytes.iter().enumerate().filter(|(_, &b)| b == b'\n').map(|(i, _)| i + 1);
        Lines { starts: std::iter::once(0).chain(ends).collect() }
    }

    /// The line, from 1, of the byte at `offset`.
    pub fn line(&self, offset: usize) -> u32 {
        u32::try_from(self.starts.partition_point(|&s| s <= offset)).unwrap_or(u32::MAX)
    }

    /// The column, from 0, in bytes, of the byte at `offset`.
    pub fn column(&self, offset: usize) -> u32 {
        let line = self.starts.partition_point(|&s| s <= offset).saturating_sub(1);
        u32::try_from(offset - self.starts[line]).unwrap_or(u32::MAX)
    }

    /// How many lines the file has, its last one counted though no newline ends it.
    pub fn count(&self) -> u32 {
        u32::try_from(self.starts.len()).unwrap_or(u32::MAX)
    }
}

/// An item that holds a `fn`, as the fn's qualified name and the resolution of its calls read it.
#[derive(Clone, Debug)]
pub enum Holder {
    Mod(String),
    /// An `impl`: its type and its trait, each as written, without the spaces Rust allows around punctuation.
    Impl { ty: String, trait_: Option<String> },
    Trait(String),
    Fn(String),
    Const(String),
    Static(String),
}

/// An attribute, outer or inner, as the parser places it; doc comments are none.
#[derive(Clone, Debug)]
pub struct Attr {
    /// Its path, as `should_panic` or `tokio::test`.
    pub path: String,
    /// The attribute, from its `#` to its `]`, as written.
    pub text: String,
    /// The line its `#` is on.
    pub line: u32,
}

/// A value a `fn` throws away: `let _ = value;`, `_ = value;` or `value.ok();`.
#[derive(Clone, Debug)]
pub struct Discard {
    /// The statement as written in the finding, with `{}` where the value goes.
    pub written: &'static str,
    /// The lines of the statement that throws it away, from its `let` or value to its `;`.
    pub lines: model::Lines,
    /// The value thrown away, as written.
    pub text: String,
}

/// A `fn`, in a trait without a body included.
#[derive(Debug)]
pub struct Fn {
    pub name: String,
    /// The items holding it, outermost first.
    pub holders: Vec<Holder>,
    /// The line of its visibility, or of its `fn` and the qualifiers before it.
    pub line: u32,
    /// The line of its first outer attribute or doc comment; `line` when it has none.
    pub attrs_line: u32,
    pub end: u32,
    pub attrs: Vec<Attr>,
    /// The outer attributes written in its body, on its statements, expressions and the items it holds, those inside
    /// the bodies of the fns it holds left out.
    pub body_attrs: Vec<Attr>,
    /// The assertion macros its body calls, those of the closures it holds included and of the fns it holds not.
    pub asserts: Vec<Assertion>,
    pub discards: Vec<Discard>,
    /// A digest of its body's tokens; None for one without a body.
    pub body: Option<String>,
    /// For a fn with a body that no fn holds, the flow of that body and the path of each call in it, as written
    /// (`["f"]`, `["self", "f"]`, `["Type", "f"]`), the calls of the fns and closures it holds included; None for
    /// any other.
    pub flow: Option<(Vec<Flow>, Vec<Vec<String>>)>,
}

/// An inline module, `mod name { ... }`.
#[derive(Debug)]
pub struct Mod {
    pub name: String,
    /// The line of its visibility or its `mod`.
    pub line: u32,
    pub attrs_line: u32,
    pub end: u32,
    /// Its outer attributes.
    pub attrs: Vec<Attr>,
    /// Whether it is compiled only under `cfg(test)`, by a `cfg` whose predicate needs `test`.
    pub cfg_test: bool,
}

/// What [`collect`] gathers of a file.
#[derive(Debug, Default)]
pub struct Items {
    /// Every `fn`, in the order they start.
    pub fns: Vec<Fn>,
    pub mods: Vec<Mod>,
    pub bindings: Vec<Binding>,
    /// Every inner attribute, `#![...]`, wherever it is written.
    pub inner_attrs: Vec<Attr>,
}

/// What `tree`, parsed from `text`, the whole file whose line numbers `lines` gives, holds; `parser` parses macro
/// arguments.
pub fn collect(tree: &Tree, text: Text, lines: &Lines, parser: &mut Parser) -> Items {
    let mut walk = Walk { lines, parser, items: Items::default(), holders: vec![], current_fn: None };
    walk.node(tree.root_node(), text, false);
    walk.items
}

/// The walk [`collect`] makes.
struct Walk<'a> {
    lines: &'a Lines,
    parser: &'a mut Parser,
    items: Items,
    /// The items the walk is inside, outermost first.
    holders: Vec<Holder>,
    /// The index in `items.fns` of the innermost `fn` the walk is inside, which owns the assertions and discards met.
    current_fn: Option<usize>,
}

impl Walk<'_> {
    fn line(&self, text: Text, node: Node) -> u32 {
        self.lines.line(text.start(node))
    }

    /// Walks `node`; `assoc` says whether it is an item of an `impl` or a `trait`.
    fn node(&mut self, node: Node, text: Text, assoc: bool) {
        match node.kind() {
            "function_item" | "function_signature_item" => self.function(node, text),
            "foreign_mod_item" | "macro_definition" => {}
            "attribute_item" => {
                if let (Some(i), Some(attr)) = (self.current_fn, attribute(node, text, self.lines)) {
                    self.items.fns[i].body_attrs.push(attr);
                }
            }
            "inner_attribute_item" => self.items.inner_attrs.extend(attribute(node, text, self.lines)),
            "mod_item" => self.module(node, text),
            "impl_item" => {
                let compact = |n: Node| compact_tokens(n, text);
                let ty = node.child_by_field_name("type").map(compact).unwrap_or_default();
                let trait_ = node.child_by_field_name("trait").map(compact);
                self.inside(Holder::Impl { ty, trait_ }, |w| w.items_of(node, text));
            }
            "trait_item" => {
                let name = self.bind_name(node, text, false);
                self.inside(Holder::Trait(name), |w| w.items_of(node, text));
            }
            "const_item" | "static_item" => self.const_or_static(node, text, assoc),
            "struct_item" | "enum_item" | "union_item" | "type_item" | "associated_type" => {
                self.bind_name(node, text, false);
                self.children(node, text);
            }
            "use_as_clause" => {
                if let Some(alias) = node.child_by_field_name("alias") {
                    self.bind(alias, text, false);
                }
            }
            "let_declaration" => self.local(node, text),
            "expression_statement" => {
                self.statement(node, text);
                self.children(node, text);
            }
            "for_expression" => {
                self.bind_field(node, "pattern", text);
                self.children(node, text);
            }
            "match_arm" => {
                self.bind_arm_pattern(node, text);
                self.children(node, text);
            }
            "closure_expression" => {
                self.bind_closure_parameters(node, text);
                self.children(node, text);
            }
            "let_condition" => {
                self.bind_field(node, "pattern", text);
                self.children(node, text);
            }
            "macro_invocation" => self.macro_call(node, text),
            _ => self.children(node, text),
        }
    }

    fn children(&mut self, node: Node, text: Text) {
        for child in named(node) {
            self.node(child, text, false);
        }
    }

    /// Walks a `const` or `static` item, `node`, inside a holder of its name; `assoc` as [`Walk::node`] says.
    fn const_or_static(&mut self, node: Node, text: Text, assoc: bool) {
        let name = self.bind_name(node, text, false);
        // An associated const is no holder: what it holds takes no name from it.
        match (assoc, node.kind()) {
            (true, _) => self.children(node, text),
            (false, "const_item") => self.inside(Holder::Const(name), |w| w.children(node, text)),
            (false, _) => self.inside(Holder::Static(name), |w| w.children(node, text)),
        }
    }

    /// Binds the names the pattern of `arm`, a match arm, binds; its guard binds none of its own.
    fn bind_arm_pattern(&mut self, arm: Node, text: Text) {
        let Some(pattern) = arm.child_by_field_name("pattern") else { return };
        // A match_pattern: the pattern, then the guard as its `condition`.
        let guard = pattern.child_by_field_name("condition");
        for part in named(pattern).into_iter().filter(|p| Some(*p) != guard) {
            self.bind_pattern(part, text, false);
        }
    }

    /// Binds the names the parameters of `closure` bind.
    fn bind_closure_parameters(&mut self, closure: Node, text: Text) {
        let Some(params) = closure.child_by_field_name("parameters") else { return };
        for param in named(params) {
            // A typed parameter holds its pattern; an untyped one is the pattern.
            let pattern = if param.kind() == "parameter" { param.child_by_field_name("pattern") } else { Some(param) };
            if let Some(pattern) = pattern {
                self.bind_pattern(pattern, text, false);
            }
        }
    }

    /// Walks the items of an `impl` or a `trait`, `node`.
    fn items_of(&mut self, node: Node, text: Text) {
        for child in named(node) {
            if child.kind() == "declaration_list" {
                for item in named(child) {
                    self.node(item, text, true);
                }
            } else {
                self.node(child, text, false);
            }
        }
    }

    fn inside(&mut self, holder: Holder, walk: impl FnOnce(&mut Self)) {
        self.holders.push(holder);
        walk(self);
        self.holders.pop();
    }

    /// Records a name `ident` binds, unless it is `_`; `fixed` as [`Binding::fixed`] says.
    fn bind(&mut self, ident: Node, text: Text, fixed: bool) {
        let name = unraw(text.of(ident));
        if name != "_" {
            let at = text.start(ident);
            let binding =
                Binding { name: name.into(), line: self.lines.line(at), column: self.lines.column(at), fixed };
            self.items.bindings.push(binding);
        }
    }

    /// Binds the `name` of the item `node` and gives it, unraw.
    fn bind_name(&mut self, node: Node, text: Text, fixed: bool) -> String {
        match node.child_by_field_name("name") {
            Some(name) => {
                self.bind(name, text, fixed);
                unraw(text.of(name)).into()
            }
            None => String::new(),
        }
    }

    fn bind_field(&mut self, node: Node, field: &str, text: Text) {
        if let Some(pattern) = node.child_by_field_name(field) {
            self.bind_pattern(pattern, text, false);
        }
    }

    fn bind_pattern(&mut self, pattern: Node, text: Text, fixed: bool) {
        for ident in pattern_names(pattern, text) {
            self.bind(ident, text, fixed);
        }
    }

    /// Records a `fn`, `node`, and its name and parameters as bindings, then walks its body with the fn the
    /// innermost holder and the owner of the assertions and discards met.
    fn function(&mut self, node: Node, text: Text) {
        let body = node.child_by_field_name("body");
        let line = self.line(text, node);
        let (attrs_line, attrs) = outer_attributes(node, text, self.lines);
        let top = !self.holders.iter().any(|h| matches!(h, Holder::Fn(_)));
        let flow = body.filter(|_| top).map(|b| flow(b, text, self.parser));
        // A trait fixes the name and the parameters of the fns implementing it.
        let fixed = matches!(self.holders.last(), Some(Holder::Impl { trait_: Some(_), .. }));
        let name = self.bind_name(node, text, fixed);
        if let Some(params) = node.child_by_field_name("parameters") {
            for param in named(params).into_iter().filter(|p| p.kind() == "parameter") {
                if let Some(pattern) = param.child_by_field_name("pattern") {
                    self.bind_pattern(pattern, text, fixed);
                }
            }
        }
        self.items.fns.push(Fn {
            name: name.clone(),
            holders: self.holders.clone(),
            line,
            attrs_line: attrs_line.unwrap_or(line),
            end: self.lines.line(text.end(node).saturating_sub(1)),
            attrs,
            body_attrs: vec![],
            asserts: vec![],
            discards: vec![],
            body: body.map(|b| digest(&tokens(b, text).join(" "))),
            flow,
        });
        let outer = self.current_fn.replace(self.items.fns.len() - 1);
        if let Some(body) = body {
            self.inside(Holder::Fn(name), |w| w.node(body, text, false));
        }
        self.current_fn = outer;
    }

    fn module(&mut self, node: Node, text: Text) {
        let Some(body) = node.child_by_field_name("body") else { return };
        let line = self.line(text, node);
        let (attrs_line, attrs) = outer_attributes(node, text, self.lines);
        let name = node.child_by_field_name("name").map(|n| unraw(text.of(n)).to_string()).unwrap_or_default();
        self.items.mods.push(Mod {
            name: name.clone(),
            line,
            attrs_line: attrs_line.unwrap_or(line),
            end: self.lines.line(text.end(body).saturating_sub(1)),
            attrs,
            // Its inner attributes, `#![cfg(test)]` at the top of its body, mark it too.
            cfg_test: preceding_attributes(node)
                .into_iter()
                .chain(named(body).into_iter().filter(|n| n.kind() == "inner_attribute_item"))
                .any(|a| holds_only_under_test(a, text)),
        });
        self.inside(Holder::Mod(name), |w| w.node(body, text, false));
    }

    /// A `let`: binds its pattern, and records the value a `let _ = value;` throws away.
    fn local(&mut self, node: Node, text: Text) {
        let pattern = node.child_by_field_name("pattern");
        if let Some(pattern) = pattern {
            self.bind_pattern(pattern, text, false);
        }
        if let (Some(pattern), Some(init)) = (pattern, node.child_by_field_name("value")) {
            if pattern.kind() == "_" {
                self.discard("let _ = {};", node, init, text);
            }
        }
        self.children(node, text);
    }

    /// An expression statement ended by `;`: records the value `_ = value;` or `value.ok();` throws away.
    fn statement(&mut self, node: Node, text: Text) {
        let Some(expr) = node.named_child(0) else { return };
        if !children(node).iter().any(|c| c.kind() == ";") {
            return;
        }
        match expr.kind() {
            "assignment_expression" => {
                let left = expr.child_by_field_name("left");
                if let (Some(left), Some(right)) = (left, expr.child_by_field_name("right")) {
                    if left.kind() == "identifier" && text.of(left) == "_" {
                        self.discard("_ = {};", node, right, text);
                    }
                }
            }
            "call_expression" => {
                let method = expr.child_by_field_name("function").filter(|f| f.kind() == "field_expression");
                let no_args = expr.child_by_field_name("arguments").is_some_and(|a| named(a).is_empty());
                if let Some(method) = method.filter(|_| no_args) {
                    let field = method.child_by_field_name("field").map(|f| text.of(f));
                    if let (Some("ok"), Some(receiver)) = (field, method.child_by_field_name("value")) {
                        self.discard("{}.ok();", node, receiver, text);
                    }
                }
            }
            _ => {}
        }
    }

    /// Records that `statement`, written as `written`, throws away the value `thrown`, in the fn the walk is in.
    fn discard(&mut self, written: &'static str, statement: Node, thrown: Node, text: Text) {
        if let Some(i) = self.current_fn {
            let last = self.lines.line(text.end(statement).saturating_sub(1));
            let lines = model::Lines { first: self.line(text, statement), last };
            let discard = Discard { written, lines, text: text.of(thrown).into() };
            self.items.fns[i].discards.push(discard);
        }
    }

    /// A macro call: an assertion of the fn it is in, when an assertion macro; then its arguments, as code.
    fn macro_call(&mut self, node: Node, text: Text) {
        let name = node.child_by_field_name("macro").map(|m| unraw(text.of(macro_name(m))).to_string());
        if let (Some(owner), Some(name)) = (self.current_fn, name) {
            if ASSERTION_MACROS.contains(&name.as_str()) {
                let mut literals = vec![];
                let blanked = node.named_children(&mut node.walk()).find(|c| c.kind() == "token_tree").map_or(
                    String::new(),
                    |tree| {
                        let mut tokens = vec![];
                        blank_literals(tree, text, &mut tokens, &mut literals);
                        tokens.join(" ")
                    },
                );
                let assertion = Assertion {
                    line: self.line(text, node),
                    text: text.of(node).into(),
                    shape: format!("{name}! {blanked}"),
                    literals,
                    reads: vec![],
                };
                self.items.fns[owner].asserts.push(assertion);
            }
        }
        for (tree, bytes, shift) in macro_args(node, text, self.parser) {
            let arg_text = Text { bytes: &bytes, shift };
            if let Some(arg) = wrapped_arg(&tree) {
                self.node(arg, arg_text, false);
            }
        }
    }
}

/// The flow of a `fn`'s `body`, and the path of each call in it, the calls of the fns and closures it holds included.
fn flow(body: Node, text: Text, parser: &mut Parser) -> (Vec<Flow>, Vec<Vec<String>>) {
    let mut walker = FlowWalk { parser, calls: vec![], held: vec![] };
    let mut out = vec![];
    walker.node(body, text, &mut out);
    walker.calls.append(&mut walker.held);
    (out, walker.calls)
}

/// The walk [`flow`] makes.
struct FlowWalk<'a> {
    parser: &'a mut Parser,
    /// The path of each call met in the `fn` the walk is in, those of the closures it holds included and those of the
    /// fns it holds not, so that a held fn is recursive by its own calls alone.
    calls: Vec<Vec<String>>,
    /// The path of each call of the fns that the `fn` the walk is in holds, at any depth.
    held: Vec<Vec<String>>,
}

impl FlowWalk<'_> {
    /// The flow `node` holds, appended to `out`.
    fn node(&mut self, node: Node, text: Text, out: &mut Vec<Flow>) {
        match node.kind() {
            "if_expression" => self.if_chain(node, text, out),
            "let_declaration" => self.let_else(node, text, out),
            "match_expression" => self.match_flow(node, text, out),
            "loop_expression" | "while_expression" | "for_expression" => {
                let head = node.child_by_field_name(if node.kind() == "for_expression" { "value" } else { "condition" });
                let level = head.map(|h| self.part(h, text)).unwrap_or_default();
                let nested = node.child_by_field_name("body").map(|b| self.part(b, text)).unwrap_or_default();
                out.push(Flow::Increment { nesting: true, level, nested });
            }
            "break_expression" | "continue_expression" => self.jump(node, text, out),
            "binary_expression" | "let_chain" if logical(node, text).is_some() => {
                let (mut ands, mut operands) = (vec![], vec![]);
                logical_run(node, text, &mut ands, &mut operands);
                let mut held = vec![];
                self.each(operands, text, &mut held);
                out.push(Flow::Booleans { ands, operands: held });
            }
            "closure_expression" => {
                let body = node.child_by_field_name("body").map(|b| self.part(b, text)).unwrap_or_default();
                out.push(Flow::Nesting { recursive: false, body });
            }
            "function_item" => self.nested_fn(node, text, false, out),
            "impl_item" | "trait_item" => self.items_of(node, text, out),
            "call_expression" => {
                if let Some(path) = node.child_by_field_name("function").and_then(|f| call_path(f, text)) {
                    self.calls.push(path);
                }
                self.children(node, text, out);
            }
            "macro_invocation" => self.macro_call(node, text, out),
            "foreign_mod_item" | "macro_definition" | "attribute_item" | "inner_attribute_item"
            | "function_signature_item" => {}
            _ => self.children(node, text, out),
        }
    }

    fn children(&mut self, node: Node, text: Text, out: &mut Vec<Flow>) {
        self.each(named(node), text, out);
    }

    fn each(&mut self, nodes: Vec<Node>, text: Text, out: &mut Vec<Flow>) {
        for node in nodes {
            self.node(node, text, out);
        }
    }

    /// A `let`, `node`: its value, and its `else` block, which nests, when it is a let-else.
    fn let_else(&mut self, node: Node, text: Text, out: &mut Vec<Flow>) {
        if let Some(value) = node.child_by_field_name("value") {
            self.node(value, text, out);
        }
        if let Some(otherwise) = node.child_by_field_name("alternative") {
            let nested = self.part(otherwise, text);
            out.push(Flow::Increment { nesting: true, level: vec![], nested });
        }
    }

    /// A `match`, `node`: one increment for all its arms, its scrutinee at its level and each arm's guard and value
    /// nested.
    fn match_flow(&mut self, node: Node, text: Text, out: &mut Vec<Flow>) {
        let level = node.child_by_field_name("value").map(|v| self.part(v, text)).unwrap_or_default();
        let mut nested = vec![];
        let arms = node.child_by_field_name("body").map(named).unwrap_or_default();
        for arm in arms.into_iter().filter(|a| a.kind() == "match_arm") {
            let guard = arm.child_by_field_name("pattern").and_then(|p| p.child_by_field_name("condition"));
            for part in guard.into_iter().chain(arm.child_by_field_name("value")) {
                self.node(part, text, &mut nested);
            }
        }
        out.push(Flow::Increment { nesting: true, level, nested });
    }

    /// A `break` or `continue`, `node`: an increment when it names a label, and the flow of its value.
    fn jump(&mut self, node: Node, text: Text, out: &mut Vec<Flow>) {
        let (labels, values): (Vec<Node>, Vec<Node>) = named(node).into_iter().partition(|p| p.kind() == "label");
        if !labels.is_empty() {
            out.push(Flow::Increment { nesting: false, level: vec![], nested: vec![] });
        }
        self.each(values, text, out);
    }

    /// The items of an `impl` or a `trait`, `node`: each fn a method the walked fn holds.
    fn items_of(&mut self, node: Node, text: Text, out: &mut Vec<Flow>) {
        for item in node.child_by_field_name("body").map(named).unwrap_or_default() {
            if item.kind() == "function_item" {
                self.nested_fn(item, text, true, out);
            } else {
                self.node(item, text, out);
            }
        }
    }

    /// The arguments of a macro call, `node`, each that reads as an expression read as one.
    fn macro_call(&mut self, node: Node, text: Text, out: &mut Vec<Flow>) {
        for (tree, bytes, shift) in macro_args(node, text, self.parser) {
            if let Some(arg) = wrapped_arg(&tree) {
                self.node(arg, Text { bytes: &bytes, shift }, out);
            }
        }
    }

    /// The flow `node` holds, as one part of a node of flow.
    fn part(&mut self, node: Node, text: Text) -> Vec<Flow> {
        let mut out = vec![];
        self.node(node, text, &mut out);
        out
    }

    /// An `if`, with the `else if`s and the `else` that follow it, each an increment of its own at the `if`'s level.
    fn if_chain(&mut self, node: Node, text: Text, out: &mut Vec<Flow>) {
        let mut node = node;
        let mut nesting = true;
        loop {
            let level = node.child_by_field_name("condition").map(|c| self.part(c, text)).unwrap_or_default();
            let nested = node.child_by_field_name("consequence").map(|c| self.part(c, text)).unwrap_or_default();
            out.push(Flow::Increment { nesting, level, nested });
            nesting = false;
            let Some(otherwise) = node.child_by_field_name("alternative").and_then(|e| e.named_child(0)) else {
                return;
            };
            if otherwise.kind() == "if_expression" {
                node = otherwise;
            } else {
                let nested = self.part(otherwise, text);
                out.push(Flow::Increment { nesting: false, level: vec![], nested });
                return;
            }
        }
    }

    /// A fn the walked one holds, `node`; `method` says whether it is an `impl`'s or a `trait`'s.
    fn nested_fn(&mut self, node: Node, text: Text, method: bool, out: &mut Vec<Flow>) {
        let Some(body) = node.child_by_field_name("body") else { return };
        let (outer, outer_held) = (std::mem::take(&mut self.calls), std::mem::take(&mut self.held));
        let body = self.part(body, text);
        let own = std::mem::replace(&mut self.calls, outer);
        let deeper = std::mem::replace(&mut self.held, outer_held);
        let name = node.child_by_field_name("name").map(|n| unraw(text.of(n))).unwrap_or_default();
        let itself = |path: &Vec<String>| match path.as_slice() {
            [first, called] if method => (first == "self" || first == "Self") && called == name,
            [called] if !method => called == name,
            _ => false,
        };
        out.push(Flow::Nesting { recursive: own.iter().any(itself), body });
        self.held.extend(own);
        self.held.extend(deeper);
    }
}

/// Whether `node` is a `&&` or `||` and which: true for `&&`; a let chain is all `&&`. None for any other node.
fn logical(node: Node, text: Text) -> Option<bool> {
    match node.kind() {
        "let_chain" => Some(true),
        "binary_expression" => match node.child_by_field_name("operator").map(|o| text.of(o)) {
            Some("&&") => Some(true),
            Some("||") => Some(false),
            _ => None,
        },
        _ => None,
    }
}

/// Appends to `ands` the operators of the run of `&&` and `||` that `node`, one of them or a let chain, starts,
/// through parentheses, in the order they are written, and to `operands` what they join.
fn logical_run<'t>(node: Node<'t>, text: Text, ands: &mut Vec<bool>, operands: &mut Vec<Node<'t>>) {
    if node.kind() == "let_chain" {
        for part in children(node) {
            if part.kind() == "&&" {
                ands.push(true);
            } else if part.is_named() && !part.is_extra() {
                logical_operand(part, text, ands, operands);
            }
        }
        return;
    }
    if let (Some(left), Some(right)) = (node.child_by_field_name("left"), node.child_by_field_name("right")) {
        logical_operand(left, text, ands, operands);
        ands.push(logical(node, text) == Some(true));
        logical_operand(right, text, ands, operands);
    }
}

fn logical_operand<'t>(node: Node<'t>, text: Text, ands: &mut Vec<bool>, operands: &mut Vec<Node<'t>>) {
    if node.kind() == "parenthesized_expression" {
        if let Some(inner) = named(node).into_iter().next() {
            return logical_operand(inner, text, ands, operands);
        }
    }
    if logical(node, text).is_some() {
        logical_run(node, text, ands, operands);
    } else {
        operands.push(node);
    }
}

/// The path a call's `function` names it by, as written, when it is a path or a method of `self`: `["f"]`,
/// `["Type", "f"]`, `["self", "f"]`; None for any other callee, such as `<T as Trait>::f` or a closure in a field.
fn call_path(function: Node, text: Text) -> Option<Vec<String>> {
    let function = if function.kind() == "generic_function" { function.child_by_field_name("function")? } else { function };
    if function.kind() == "field_expression" {
        let receiver = function.child_by_field_name("value")?;
        let method = function.child_by_field_name("field")?;
        return (receiver.kind() == "self").then(|| vec!["self".into(), unraw(text.of(method)).into()]);
    }
    let mut path = vec![];
    let mut at = Some(function);
    while let Some(node) = at {
        match node.kind() {
            "identifier" | "self" | "super" | "crate" | "type_identifier" => {
                path.push(unraw(text.of(node)).into());
                at = None;
            }
            "scoped_identifier" | "scoped_type_identifier" => {
                path.push(unraw(text.of(node.child_by_field_name("name")?)).into());
                at = node.child_by_field_name("path");
            }
            "generic_type" => at = node.child_by_field_name("type"),
            _ => return None,
        }
    }
    path.reverse();
    Some(path)
}

/// Each argument of the macro call `node`, a tree of its own of the argument wrapped as `ARG_PREFIX` says, with the
/// text it was parsed from and its shift (see [`Text`]); none unless every argument is an expression and they are
/// separated by commas, a trailing one allowed, as syn's `Punctuated<Expr, Token![,]>` reads them.
fn macro_args(node: Node, text: Text, parser: &mut Parser) -> Vec<(Tree, Vec<u8>, isize)> {
    let Some(tree) = named(node).into_iter().find(|c| c.kind() == "token_tree") else { return vec![] };
    let inside = children(tree);
    let inside = inside.get(1..inside.len().saturating_sub(1)).unwrap_or_default();
    // Each argument's tokens, comments left out.
    let mut args: Vec<Vec<Node>> = vec![vec![]];
    for token in inside {
        if token.kind() == "," {
            args.push(vec![]);
        } else if let (false, Some(arg)) = (token.is_extra(), args.last_mut()) {
            arg.push(*token);
        }
    }
    if args.last().is_some_and(Vec::is_empty) {
        args.pop();
    }
    let mut out = vec![];
    for arg in args {
        let (Some(first), Some(last)) = (arg.first(), arg.last()) else { return vec![] };
        let mut bytes = ARG_PREFIX.to_vec();
        bytes.extend_from_slice(&text.bytes[first.start_byte()..last.end_byte()]);
        bytes.extend_from_slice(ARG_SUFFIX);
        let shift = text.shift + first.start_byte() as isize - ARG_PREFIX.len() as isize;
        let Some(parsed) = parser.parse(&bytes, None) else { return vec![] };
        if parsed.root_node().has_error() || wrapped_arg(&parsed).is_none() {
            return vec![];
        }
        out.push((parsed, bytes, shift));
    }
    out
}

/// The one expression in the parentheses of a macro argument parsed as [`macro_args`] wraps it; None when the
/// parentheses hold other than one.
fn wrapped_arg(tree: &Tree) -> Option<Node<'_>> {
    let function = named(tree.root_node()).into_iter().find(|n| n.kind() == "function_item")?;
    let body = named(function.child_by_field_name("body")?).into_iter().filter(|n| !n.is_extra()).collect::<Vec<_>>();
    let [statement] = body.as_slice() else { return None };
    let parens = statement.named_child(0).filter(|p| p.kind() == "parenthesized_expression")?;
    let inner: Vec<Node> = named(parens).into_iter().filter(|n| !n.is_extra()).collect();
    match inner.as_slice() {
        [expr] => Some(*expr),
        _ => None,
    }
}

/// The path of a macro call's `macro` field, `node`: the name its last segment gives.
fn macro_name(node: Node) -> Node {
    if node.kind() == "scoped_identifier" {
        node.child_by_field_name("name").unwrap_or(node)
    } else {
        node
    }
}

/// The identifiers `pattern` binds, in the order they are written: each identifier pattern in it but those that
/// start with an uppercase letter, which name a constant or a unit variant, such as `None`, rather than bind one.
fn pattern_names<'t>(pattern: Node<'t>, text: Text) -> Vec<Node<'t>> {
    let mut out = vec![];
    // Patterns still to read, the next one last.
    let mut pending = vec![pattern];
    while let Some(node) = pending.pop() {
        match node.kind() {
            "identifier" | "shorthand_field_identifier" => {
                if !unraw(text.of(node)).starts_with(char::is_uppercase) {
                    out.push(node);
                }
            }
            "scoped_identifier" | "scoped_type_identifier" | "generic_type" | "type_identifier" | "macro_invocation"
            | "range_pattern" | "const_block" | "field_identifier" => {}
            _ => pending.extend(sub_patterns(node).into_iter().rev()),
        }
    }
    out
}

/// The named children of `node`, a pattern, in order, but its `type`: the path of a tuple-struct or struct pattern,
/// which names a type, not a binding.
fn sub_patterns(node: Node) -> Vec<Node> {
    let mut out = vec![];
    let mut cursor = node.walk();
    if !cursor.goto_first_child() {
        return out;
    }
    loop {
        if cursor.node().is_named() && cursor.field_name() != Some("type") {
            out.push(cursor.node());
        }
        if !cursor.goto_next_sibling() {
            return out;
        }
    }
}

/// The attribute items right before the item `node`, in order: tree-sitter makes them its siblings.
pub fn preceding_attributes(node: Node) -> Vec<Node> {
    let mut out = vec![];
    let mut at = node.prev_sibling();
    while let Some(sibling) = at {
        match sibling.kind() {
            "attribute_item" => out.push(sibling),
            "line_comment" | "block_comment" => {}
            _ => break,
        }
        at = sibling.prev_sibling();
    }
    out.reverse();
    out
}

/// The line of the first of the item `node`'s outer attributes, doc comments included, None when it has none; and
/// those attributes but the doc comments.
fn outer_attributes(node: Node, text: Text, lines: &Lines) -> (Option<u32>, Vec<Attr>) {
    let mut first = None;
    let mut at = node.prev_sibling();
    while let Some(sibling) = at {
        let doc = matches!(sibling.kind(), "line_comment" | "block_comment")
            && sibling.child_by_field_name("outer").is_some();
        match sibling.kind() {
            "attribute_item" => first = Some(sibling),
            "line_comment" | "block_comment" if doc => first = Some(sibling),
            "line_comment" | "block_comment" => {}
            _ => break,
        }
        at = sibling.prev_sibling();
    }
    let attrs = preceding_attributes(node).into_iter().filter_map(|item| attribute(item, text, lines)).collect();
    (first.map(|f| lines.line(text.start(f))), attrs)
}

/// The attribute item `item`, an `attribute_item` or an `inner_attribute_item`, as an [`Attr`]; None for a `doc`
/// attribute or one the parser gives no path.
fn attribute(item: Node, text: Text, lines: &Lines) -> Option<Attr> {
    let attribute = named(item).into_iter().find(|c| c.kind() == "attribute")?;
    let path = path_segments(attribute.named_child(0)?, text).join("::");
    (path != "doc").then(|| Attr { path, text: text.of(item).into(), line: lines.line(text.start(item)) })
}

/// The segments of the path `node`, unraw, outermost first.
fn path_segments(node: Node, text: Text) -> Vec<String> {
    let mut out = vec![];
    let mut at = Some(node);
    while let Some(node) = at {
        if node.kind() == "scoped_identifier" {
            out.extend(node.child_by_field_name("name").map(|n| unraw(text.of(n)).to_string()));
            at = node.child_by_field_name("path");
        } else {
            out.push(unraw(text.of(node)).into());
            at = None;
        }
    }
    out.reverse();
    out
}

/// The syntax tree of the Rust source `source`; None where it is not UTF-8 or tree-sitter finds an error in it.
pub fn parse_whole(source: &[u8]) -> Option<Tree> {
    std::str::from_utf8(source).ok()?;
    let mut parser = Parser::new();
    parser.set_language(&tree_sitter_rust::LANGUAGE.into()).ok()?;
    parser.parse(source, None).filter(|tree| !tree.root_node().has_error())
}

/// Whether `body`, a file's or an inline module's, opens with `#![cfg(test)]`, making the whole of it test code.
pub fn opens_test_only(body: Node, text: Text) -> bool {
    named(body).into_iter().any(|n| n.kind() == "inner_attribute_item" && holds_only_under_test(n, text))
}

/// Whether the attribute item `item` is a `cfg` whose predicate holds only under `cfg(test)`: `test`, or an `all`
/// with such an argument.
pub fn holds_only_under_test(item: Node, text: Text) -> bool {
    let Some(attribute) = named(item).into_iter().find(|c| c.kind() == "attribute") else { return false };
    let path = attribute.named_child(0).map(|p| text.of(p));
    let arguments = attribute.child_by_field_name("arguments");
    match (path, arguments) {
        (Some("cfg"), Some(arguments)) => {
            let tokens: Vec<Node> = children(arguments).into_iter().filter(|t| !t.is_extra()).collect();
            // `(` predicate `)`
            match tokens.as_slice() {
                [_, inner @ .., _] => parse_meta(inner, text).is_some_and(|(needs, rest)| needs && rest.is_empty()),
                _ => false,
            }
        }
        _ => false,
    }
}

/// Reads one cfg predicate off the front of `tokens`, a token tree's tokens: whether it needs `test`, and the tokens
/// after it; None when they do not start with one, as syn's `Meta` reads it.
fn parse_meta<'n, 't>(tokens: &'n [Node<'t>], text: Text) -> Option<(bool, &'n [Node<'t>])> {
    let (name, rest) = tokens.split_first()?;
    if name.kind() != "identifier" {
        return None;
    }
    let name = text.of(*name);
    match rest.split_first() {
        Some((list, rest)) if list.kind() == "token_tree" => {
            if name != "all" {
                return Some((false, rest));
            }
            let inner: Vec<Node> = children(*list).into_iter().filter(|t| !t.is_extra()).collect();
            let mut args = inner.get(1..inner.len().saturating_sub(1)).unwrap_or_default();
            let mut needs = false;
            while !args.is_empty() {
                let (arg_needs, after) = parse_meta(args, text)?;
                needs |= arg_needs;
                args = match after.split_first() {
                    Some((comma, after)) if comma.kind() == "," => after,
                    None => after,
                    Some(_) => return None,
                };
            }
            Some((needs, rest))
        }
        Some((eq, rest)) if eq.kind() == "=" => {
            let (_, rest) = rest.split_first()?;
            Some((false, rest))
        }
        _ => Some((name == "test", rest)),
    }
}

/// The tokens of `node` as written, comments left out and each literal one token.
pub fn tokens<'t>(node: Node, text: Text<'t>) -> Vec<&'t str> {
    let mut out = vec![];
    let mut pending = vec![node];
    while let Some(node) = pending.pop() {
        if node.is_extra() && matches!(node.kind(), "line_comment" | "block_comment") {
            continue;
        }
        if node.child_count() == 0 || is_literal(node) {
            out.push(text.of(node));
            continue;
        }
        pending.extend(children(node).into_iter().rev());
    }
    out
}

/// `node`'s tokens, as [`tokens`] gives them, written with a space only between two that would otherwise run into
/// one word: `Workflow<'a>`, `fmt::Display`, `dyn Trait`.
fn compact_tokens(node: Node, text: Text) -> String {
    let word = |c: char| c.is_alphanumeric() || c == '_';
    let mut out = String::new();
    for token in tokens(node, text) {
        if out.chars().next_back().is_some_and(word) && token.chars().next().is_some_and(word) {
            out.push(' ');
        }
        out.push_str(token);
    }
    out
}

/// Appends the tokens of the token tree `node` to `tokens`, each literal in it, at any depth, as `$lit`, and each
/// literal's text to `literals`. `true` and `false` are words, as Rust's tokens have them, not literals.
fn blank_literals(node: Node, text: Text, tokens: &mut Vec<String>, literals: &mut Vec<String>) {
    for token in children(node) {
        if token.is_extra() {
            continue;
        }
        if is_literal(token) && token.kind() != "boolean_literal" {
            literals.push(text.of(token).into());
            tokens.push("$lit".into());
        } else if token.kind() == "token_tree" {
            blank_literals(token, text, tokens, literals);
        } else if token.child_count() == 0 || is_literal(token) {
            tokens.push(text.of(token).into());
        } else {
            for leaf in self::tokens(token, text) {
                tokens.push(leaf.into());
            }
        }
    }
}

fn is_literal(node: Node) -> bool {
    node.kind().ends_with("_literal")
}

/// `name` without the `r#` of a raw identifier.
pub fn unraw(name: &str) -> &str {
    name.strip_prefix("r#").unwrap_or(name)
}

/// `node`'s children, named and anonymous alike (punctuation and keywords too), in source order.
pub fn children(node: Node) -> Vec<Node> {
    node.children(&mut node.walk()).collect()
}

/// `node`'s named children, comments included.
pub fn named(node: Node) -> Vec<Node> {
    node.named_children(&mut node.walk()).collect()
}

/// 64-bit FNV-1a of `text`, as 16 hexadecimal digits (http://www.isthe.com/chongo/tech/comp/fnv/).
fn digest(text: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}
