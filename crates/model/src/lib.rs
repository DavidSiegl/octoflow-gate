//! What the gate's parser checks (crates/checks) know of one source file, whatever its language: its functions and
//! the flow of their bodies, its tests and their assertions, the skips and swallowed errors in it, the names it
//! binds and the constants its tests name; and, read across a whole tree, the imports between its files
//! ([`Import`]). A language's front end, such as crates/lang-rust, reads a file into a [`SourceFile`]; the checks
//! compare the two sides of a change through these types alone, so that nothing in them is one language's.
//!
//! Lines are numbered from 1, as git and the quality file number them.

/// One source file, on one side of a change, as its language's front end reads it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SourceFile {
    /// The functions the cognitive-complexity check judges as one, in the order they start: each function with a
    /// body that no other function holds, whatever modules, classes or impls hold it. A function nested in another is
    /// part of that one's flow ([`Flow::Nesting`]).
    pub functions: Vec<Function>,
    /// The tests, in the order they start; see [`Test`].
    pub tests: Vec<Test>,
    /// The items that can hold tests, such as Rust's inline modules or Python's test classes, in the order they
    /// start, nested ones after the one holding them. A test lies in each group whose lines hold its line.
    pub test_groups: Vec<TestGroup>,
    /// The lines that are test code rather than the code under test, such as a Rust `#[cfg(test)]` module's: the
    /// whole file when the project's settings place it among the tests. An edit outside them is an edit of the code.
    pub test_lines: Vec<Lines>,
    /// What weakens a test wherever it stands in the file: skips, platform-only cfgs and swallowed errors. The
    /// test-integrity check reports those on lines a change added.
    pub weakenings: Vec<Weakening>,
    /// Every name the file binds, in the order they are written.
    pub bindings: Vec<Binding>,
    /// The constants whose values the file spells out, which a test's assertions can name ([`Assertion::reads`]), in
    /// the order they are written; none in a language whose front end follows none.
    pub constants: Vec<Constant>,
}

/// A file the front end could not read as its language, with where and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unreadable {
    /// The line the trouble starts on; None when it is about the whole file, as for one that is not UTF-8.
    pub line: Option<u32>,
    /// Why, as a clause a finding can open with: "does not parse as Rust (...)".
    pub why: String,
}

/// A run of lines, both ends included.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Lines {
    pub first: u32,
    pub last: u32,
}

impl Lines {
    /// Whether `line` is one of these.
    pub fn holds(&self, line: u32) -> bool {
        self.first <= line && line <= self.last
    }
}

/// A function the cognitive-complexity check judges as one ([`SourceFile::functions`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Function {
    /// Its name, qualified by what holds it as its language writes such a path: `C.f` in Python,
    /// `tests::<Fixture as Drop>::drop` in Rust. Two functions of one file may share it.
    pub name: String,
    /// The line of its `def` or `fn`, where a finding on it points.
    pub line: u32,
    /// Its lines, from its first decorator or attribute to its end.
    pub lines: Lines,
    /// What its body does, without its name, where it is, its comments or its layout: equal for two functions whose
    /// bodies are the same code, so that one moved or renamed unchanged matches its old self.
    pub body: String,
    /// Whether it is test code ([`SourceFile::test_lines`]), which the check never judges, though it still pairs
    /// with its base so that a function moved unchanged among the tests keeps its own base.
    pub exempt: bool,
    /// The indexes in [`SourceFile::functions`] of the functions a call in its body may reach, those the functions and
    /// closures it holds make included, as its language resolves a call by name; a call it cannot resolve is left out.
    pub calls: Vec<usize>,
    /// The flow of its body, which [`Flow`] says how the check scores.
    pub flow: Vec<Flow>,
}

/// What breaks the linear flow of a function's body, in G. Ann Campbell's "Cognitive Complexity" (SonarSource, 2017,
/// version 1.7, <https://www.sonarsource.com/docs/CognitiveComplexity.pdf>). Each part holds the flow inside it, in
/// the order it is written; anything that breaks no flow, such as a block or a plain call, is left out and its
/// contents stand in its place.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Flow {
    /// One point: an `if`, a loop, a `match`, an `except`, a ternary; or, without `nesting`, an `else`, an `elif`
    /// or `else if`, or a `break` or `continue` to a label.
    Increment {
        /// Whether it also scores the nesting level it sits at, as an `if` does and an `else` does not.
        nesting: bool,
        /// What it holds at its own level, such as an `if`'s condition or a loop's iterator.
        level: Vec<Flow>,
        /// What it holds one level deeper, such as an `if`'s block or a `match`'s arms.
        nested: Vec<Flow>,
    },
    /// No point, but one level deeper for its body: a closure, a lambda, or a function nested in the judged one.
    Nesting {
        /// Whether it calls itself, which scores a point.
        recursive: bool,
        body: Vec<Flow>,
    },
    /// A run of `and` and `or` operators read as written, which scores one point for its first operator and one for
    /// each that differs from the one before it.
    Booleans {
        /// The operators, in the order they are written: true for `and`, false for `or`.
        ands: Vec<bool>,
        /// What they join, at the run's level.
        operands: Vec<Flow>,
    },
}

/// A test: a function its language's test runner runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Test {
    /// Its name, qualified as [`Function::name`] is; a test is matched with its old self by it.
    pub name: String,
    /// The line of its `def` or `fn`.
    pub line: u32,
    /// Its lines, from its first decorator or attribute to its end.
    pub lines: Lines,
    /// What its body does, as [`Function::body`] says: a test renamed or moved matches its old self by it.
    pub body: String,
    /// Its assertions, in the order they are written.
    pub assertions: Vec<Assertion>,
}

/// One assertion of a test.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Assertion {
    pub line: u32,
    /// The assertion as written, every line of it; a finding quotes its [`first_line`].
    pub text: String,
    /// The assertion with every literal blanked, its macro's or method's name included: equal for two assertions that
    /// differ only in their literals.
    pub shape: String,
    /// Its literals, as written, in order.
    pub literals: Vec<String>,
    /// The names it reads that the file's top level may bind, each once, in the order they are written: those that
    /// name one of [`SourceFile::constants`] when the file has such a constant. A name the test itself binds anywhere
    /// is not among them. None in a language whose front end follows no constants.
    pub reads: Vec<String>,
}

/// A constant of a file ([`SourceFile::constants`]): a name its top level binds once, to an expression that calls
/// nothing, so that the file itself says what it holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Constant {
    pub name: String,
    /// The line its assignment starts on.
    pub line: u32,
    /// The expression assigned to it, without its comments or its layout: equal for two expressions that are the same
    /// code.
    pub expression: String,
}

/// An item that can hold tests ([`SourceFile::test_groups`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TestGroup {
    /// Its own name, as a finding on it names it.
    pub name: String,
    /// The line of its keyword, `mod` or `class`, or its visibility before that.
    pub line: u32,
    /// Its lines, its attributes or decorators included.
    pub lines: Lines,
}

/// Something written in a file that weakens a test ([`SourceFile::weakenings`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Weakening {
    pub line: u32,
    pub kind: WeakeningKind,
    /// The code that weakens, whole, in a form that only that code has: the attribute or decorator with its
    /// arguments, the call, the clause or the comment. Two writings of the same code that differ only in whitespace
    /// have the same one; a front end may leave out more that does not change what the code does, as lang-go leaves
    /// out comments and lang-python comments, a string's quotes and a list's closing comma, as in [`Test::body`].
    /// test-integrity tells a test's old weakening from a new one by it.
    pub code: String,
    /// The finding's message: what was added and what it does, as "`#[ignore]` added: it skips a test or turns it
    /// off."
    pub message: String,
}

/// What a [`Weakening`] does to a test, the rule of the test-integrity finding it makes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WeakeningKind {
    /// It skips a test, turns it off, or expects it to fail.
    Skip,
    /// It turns a test off on the platforms whose code is not built, which a spec can ask for.
    PlatformOnly,
    /// It throws away the failure that would fail the test.
    SwallowedError,
}

impl WeakeningKind {
    /// The rule of the finding it makes, without the check's name.
    pub fn rule(self) -> &'static str {
        match self {
            WeakeningKind::Skip => "skip",
            WeakeningKind::PlatformOnly => "platform-only",
            WeakeningKind::SwallowedError => "swallowed-error",
        }
    }
}

/// A name the file binds: a variable, a parameter, a function, a type and the like.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Binding {
    pub name: String,
    pub line: u32,
    /// The column, from 0, in bytes, which orders two bindings of one line.
    pub column: u32,
    /// Whether something else fixes the name, as a trait fixes the names of the fns implementing it, so that the
    /// file's author did not choose it.
    pub fixed: bool,
}

/// One file's import of another module of the same project, as its language's front end resolves it against the
/// whole tree: the edge from the importing file to the imported one in the dependency-cycles check's graph. An
/// import of what the tree does not hold, a third party's, is none.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Import {
    /// The imported module's file, from the tree's top level; never the importing file itself.
    pub module: std::ffi::OsString,
    /// The line of the import, where a finding on it points if the change added it: of the imported name, for a
    /// statement that spans lines.
    pub line: u32,
    /// The lines of the statement that writes the import, both ends included, `line` among them: a change to any of
    /// them, as to the module path of `from a import (` above the name, can change what it imports. Not among them are
    /// the lines that decide whether the import is read at all, as a `#[cfg(test)]` on it or an `if TYPE_CHECKING:`
    /// above it.
    pub statement: std::ops::RangeInclusive<u32>,
    /// Whether the import runs only when a function it sits in is called, as a Python `import` inside a `def`; the
    /// modules depend on each other all the same.
    pub deferred: bool,
}

/// Each source file of one language in a tree, by its path from the tree's top level, with its imports of the tree's
/// other files in the order they are written; a file that imports none of them maps to none.
pub type ImportGraph = std::collections::BTreeMap<std::ffi::OsString, Vec<Import>>;

/// `text` with every whitespace character left out: the same for two layouts of code, as for [`Weakening::code`].
pub fn without_whitespace(text: &str) -> String {
    text.chars().filter(|c| !c.is_whitespace()).collect()
}

/// `text`'s first line, clipped to 160 characters with an ellipsis, as a finding's message quotes code.
pub fn first_line(text: &str) -> String {
    let line = text.split('\n').next().unwrap_or_default();
    if line.chars().count() <= 160 {
        return line.into();
    }
    let mut out: String = line.chars().take(159).collect();
    out.push('…');
    out
}

/// One token of a file's code as the duplicated-blocks check compares it, its language's front end having left out
/// comments and imports. Two runs of tokens with equal shapes are the same code but for the names and literals in
/// them: a name stands for what kind of name it is, a literal for what kind of literal, and every other token for
/// its own text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CodeToken {
    /// Equal for two tokens the check takes for the same, as a 64-bit hash: different shapes are different tokens,
    /// but for the rare collision.
    pub shape: u64,
    /// The line, from 1, the token starts on.
    pub line: u32,
}

/// The [`CodeToken::shape`] of a token written as `text` after normalising: 64-bit FNV-1a
/// (<http://www.isthe.com/chongo/tech/comp/fnv/>), so that each front end gives one text one shape.
pub fn token_shape(text: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}
