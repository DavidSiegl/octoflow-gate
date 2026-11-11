//! The flaky-tests check over a real git repository, its tests rerun with the workspace's own pytest
//! (common::python), through the `gate` binary and its quality.json. Where a test needs pytest or the system to go
//! wrong, it gives the binary a `--python` that is a shell script, or a TMPDIR. The binary's seeds and timeout are
//! fixed, so each test has all five seeds, and the run that outlives its timeout is pytest.rs's own test; the reruns
//! that stop at a run that went wrong stop here at one that exits with a status the check does not expect.

use std::ffi::OsString;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use common::{fake_python, logging_python, python_after, Repo};

mod common;

const BASE: &str = "\
import os
import unittest

# test_write_<k> adds k; test_read_<k> passes only before it.
WRITTEN = set()
# A character for each run of TestRuns.test_alternates in this checkout.
RUNS = os.path.join(os.path.dirname(__file__), 'runs')


def test_old_reads():
    assert 0 not in WRITTEN


class TestRuns(unittest.TestCase):
    def test_alternates(self):
        self.skipTest('not yet')
        with open(RUNS, 'a') as f:
            f.write('x')
        with open(RUNS) as f:
            self.assertEqual(len(f.read()) % 2, 1)
";

/// The line of [`BASE`] that keeps TestRuns.test_alternates, whose def is on line 15, from alternating.
const SKIP: &str = "self.skipTest('not yet')";

/// The file tests/test_state.py holds [`BASE`] in.
const STATE: &str = "tests/test_state.py";

/// The reader and writer pairs [`change_the_tests`] adds.
const PAIRS: usize = 8;

/// What every `mixed-results` message ends with.
const DEPENDS: &str = "it depends on the order the tests run in, or on state or randomness that one run does not share \
                       with another.";

/// A module-level test like TestRuns.test_alternates, failing on every second run, under `above`, the lines right
/// above its def, which is on line 8 without them.
fn alternating_test(name_and_parameters: &str, above: &str) -> String {
    format!(
        "import os\n\nimport pytest\n\nRUNS = os.path.join(os.path.dirname(__file__), 'runs')\n\n\n{above}\
         def {name_and_parameters}:\n    with open(RUNS, 'a') as f:\n        f.write('x')\n    \
         with open(RUNS) as f:\n        assert len(f.read()) % 2 == 1\n"
    )
}

/// A repository whose base holds [`BASE`] and a module of code, and runs neither ruff nor mypy.
fn repo() -> Repo {
    Repo::with(&[
        common::NO_PYTHON_LINTERS,
        ("pyproject.toml", b"[tool.ruff]\n"),
        (STATE, BASE.as_bytes()),
        ("calc.py", b"def add(a, b):\n    return a + b\n"),
    ])
}

/// [`BASE`] with TestRuns.test_alternates failing on every second run, whatever the order.
fn alternating() -> String {
    BASE.replace(SKIP, "self.assertIsNotNone(RUNS)")
}

/// Edits TestRuns.test_alternates to fail on every second run, and adds [`PAIRS`] writers and readers, a stable test
/// and a broken one; gives back the new text of the test file.
fn change_the_tests(repo: &Repo) -> String {
    let mut text = alternating();
    for k in 0..PAIRS {
        text += &format!("\n\ndef test_write_{k}():\n    WRITTEN.add({k})\n");
        text += &format!("\n\ndef test_read_{k}():\n    assert {k} not in WRITTEN\n");
    }
    text += "\n\ndef test_stable():\n    assert WRITTEN == WRITTEN\n\n\ndef test_broken():\n    assert False\n";
    repo.put(STATE, text.as_bytes());
    text
}

/// The line of `text` that is `definition`, from 1.
fn line_of(text: &str, definition: &str) -> u32 {
    u32::try_from(text.lines().position(|line| line == definition).unwrap()).unwrap() + 1
}

/// A finding of the check: its path, line, own rule without the check's name, and message.
type Found = (Option<String>, Option<u32>, String, String);

/// The check's findings, each a warning, on the change from HEAD to the working tree of `repo`, every file in it
/// tracked, with the gate given `python` as the project's Python and `env` in its environment.
fn findings_with(repo: &Repo, python: &Path, env: &[(&str, OsString)]) -> Vec<Found> {
    repo.git(&["add", "-A"]);
    let json = repo.outside.join("quality.json");
    let mut command = repo.gate_command("HEAD");
    command.arg("--python").arg(python).arg("--json").arg(&json);
    command.args(["--mutation-budget", "1", "--base-run-budget", "1"]);
    let done = command.envs(env.iter().map(|(k, v)| (k, v))).output().unwrap();
    assert!(matches!(done.status.code(), Some(0 | 1)), "{}", String::from_utf8_lossy(&done.stderr));
    let report: serde_json::Value = serde_json::from_slice(&std::fs::read(&json).unwrap()).unwrap();
    let mut out = vec![];
    for f in report["findings"].as_array().unwrap() {
        let Some(rule) = f["rule"].as_str().unwrap().strip_prefix("flaky-tests ") else { continue };
        assert_eq!(f["severity"], "warning", "{f}");
        let path = f["path"].as_str().map(String::from);
        let line = f["line"].as_u64().map(|n| u32::try_from(n).unwrap());
        out.push((path, line, rule.to_string(), f["message"].as_str().unwrap().to_string()));
    }
    out
}

/// [`findings_with`] the workspace's Python and the tests' own environment.
fn findings(repo: &Repo) -> Vec<Found> {
    findings_with(repo, &common::python(), &[])
}

/// The path, line and rule of each of [`findings`].
fn found(repo: &Repo) -> Vec<(Option<String>, Option<u32>, String)> {
    findings(repo).into_iter().map(|(path, line, rule, _)| (path, line, rule)).collect()
}

/// A finding's path, line and rule, as [`found`] gives them.
fn at(path: &str, line: u32, rule: &str) -> (Option<String>, Option<u32>, String) {
    (Some(path.into()), Some(line), rule.into())
}

/// The message on TestRuns.test_alternates, or on the test `name` that alternates as it does: it fails on the second
/// and fourth run.
fn alternates(name: &str) -> String {
    format!("`{name}` failed under seeds 2 and 4 and passed under seeds 1, 3 and 5: {DEPENDS}")
}

#[test]
fn an_order_dependent_test_is_reported_with_the_seeds_it_failed_under() {
    let repo = repo();
    let text = change_the_tests(&repo);
    let mut by_line = std::collections::BTreeMap::new();
    for (path, line, rule, message) in findings(&repo) {
        assert_eq!(path.as_deref(), Some(STATE));
        by_line.insert(line.unwrap(), (rule, message));
    }
    // test_alternates fails on the second and fourth run, in any order of the tests.
    assert_eq!(by_line.remove(&15), Some(("mixed-results".into(), alternates("TestRuns.test_alternates"))));
    // test_broken fails under every seed: not flaky, but reported, since the copy may be what it lacks.
    assert_eq!(by_line.remove(&line_of(&text, "def test_broken():")).unwrap().0, "failed-every-run");
    // Each seed orders each pair one way or the other. Five seeds that split none of eight independent pairs have a
    // chance of 16 ** -8 under a uniform shuffle, so some reader is reported whatever pytest-randomly's version. No
    // writer is, nor test_stable. test_old_reads depends on test_write_0 as test_read_0 does, but the change left it
    // alone, so it is not rerun.
    assert!(!by_line.is_empty());
    let numbers = |seeds: &str| -> Vec<u32> {
        seeds.split(|c: char| !c.is_ascii_digit()).filter(|n| !n.is_empty()).map(|n| n.parse().unwrap()).collect()
    };
    for (line, (rule, message)) in by_line {
        assert_eq!(rule, "mixed-results", "{message}");
        let k = (0..PAIRS).find(|k| line_of(&text, &format!("def test_read_{k}():")) == line).expect(&message);
        let rest = message.strip_prefix(&format!("`test_read_{k}` failed under seed")).expect(&message);
        let (failed, rest) = rest.split_once(" and passed under seed").expect(&message);
        let (passed, rest) = rest.split_once(": ").expect(&message);
        assert_eq!(rest, DEPENDS);
        let (failed, passed) = (numbers(failed), numbers(passed));
        assert!(!failed.is_empty() && !passed.is_empty(), "{message}");
        let mut all = [failed, passed].concat();
        all.sort_unstable();
        assert_eq!(all, [1, 2, 3, 4, 5], "{message}");
    }
}

#[test]
fn it_reports_without_failing_the_gate() {
    let repo = repo();
    repo.put(STATE, alternating().as_bytes());
    let json = repo.outside.join("quality.json");
    let mut command = repo.gate_command("HEAD");
    command.arg("--python").arg(common::python()).arg("--json").arg(&json);
    let done = command.output().unwrap();
    assert_eq!(done.status.code(), Some(0), "{}", String::from_utf8_lossy(&done.stderr));
    assert_eq!(found(&repo), [at(STATE, 15, "mixed-results")]);
}

#[test]
fn the_project_s_verbosity_does_not_hide_the_tests() {
    let repo = repo();
    repo.put(STATE, alternating().as_bytes());
    let cases = [("addopts = -v", ""), ("addopts = -qq", ""), ("", "-q"), ("verbosity_test_cases = 2", "")];
    for (ini, addopts) in cases {
        repo.put("pytest.ini", format!("[pytest]\n{ini}\n").as_bytes());
        let found = findings_with(&repo, &common::python(), &[("PYTEST_ADDOPTS", addopts.into())]);
        let messages: Vec<&str> = found.iter().map(|(.., message)| message.as_str()).collect();
        assert_eq!(messages, [alternates("TestRuns.test_alternates")], "{ini:?} {addopts:?}");
    }
}

#[test]
fn a_server_a_test_leaves_holding_the_output_does_not_hold_up_the_reruns() {
    // Under -s a test's processes inherit pytest's own output; this one outlives every run, in a session of its own,
    // so the gate cannot kill it with the run, and waits only so long for what is left of the output.
    let repo = repo();
    let pids = repo.outside.join("servers");
    repo.commit(&[("pytest.ini", b"[pytest]\naddopts = -s\n")], "-s");
    let test = format!(
        "import subprocess\nimport sys\n\n\ndef test_starts_a_server():\n    \
         sleeps = [sys.executable, '-c', 'import time; time.sleep(300)']\n    \
         server = subprocess.Popen(sleeps, start_new_session=True)\n    \
         with open('{}', 'a') as f:\n        f.write(f'{{server.pid}}\\n')\n",
        pids.display()
    );
    repo.put("tests/test_server.py", test.as_bytes());
    let started = Instant::now();
    let found = findings(&repo);
    let took = started.elapsed();
    let servers = std::fs::read_to_string(&pids).unwrap_or_default();
    for pid in servers.split_whitespace() {
        let _ = Command::new("kill").args(["-9", pid]).status();
    }
    assert_eq!(found, vec![]);
    // One server for each seed's run, so the reruns did happen, and none waited for its timeout of 30 seconds.
    assert_eq!(servers.split_whitespace().count(), 5);
    assert!(took < Duration::from_secs(60), "{took:?}");
}

#[test]
fn a_collection_the_gate_cannot_read_is_a_finding() {
    let repo = repo();
    change_the_tests(&repo);
    let python = fake_python(&repo, "printf 'tests/test_state.py: 20\\n\\n20 tests collected\\n'");
    let found = findings_with(&repo, &python, &[]);
    assert_eq!(found.len(), 1, "{found:?}");
    let (path, line, rule, message) = &found[0];
    assert_eq!((path.as_deref(), *line, rule.as_str()), (None, None, "not-rerun"));
    assert_eq!(
        message,
        "pytest, collecting the changed tests, printed no node id the gate could read: 20 tests collected; the \
         changed tests were not rerun."
    );
}

/// [`BASE`] and four tests that always fail.
fn four_failing() -> String {
    let fails = |k: u32| format!("\n\ndef test_fails_{k}():\n    assert False\n");
    BASE.to_string() + &(0..4).map(fails).collect::<String>()
}

/// Checks the findings on [`four_failing`], `text`, when each run stops at its first failure: of four tests that
/// always fail, each run reports one. None is flaky; some must be reported under fewer than two seeds, as five runs
/// report five failures in all. One that two or more runs failed is reported as failing under every seed that judged
/// it.
fn assert_too_few_runs_judged(found: &[Found], text: &str) {
    let lines: Vec<u32> = (0..4).map(|k| line_of(text, &format!("def test_fails_{k}():"))).collect();
    let too_few: Vec<&Found> = found.iter().filter(|(_, _, rule, _)| rule == "not-rerun").collect();
    assert!(!too_few.is_empty(), "{found:?}");
    assert!(found.iter().all(|(_, _, rule, _)| rule == "not-rerun" || rule == "failed-every-run"), "{found:?}");
    for (_, line, _, message) in too_few {
        let k = lines.iter().position(|at| Some(*at) == *line).expect(message);
        let rest = message.strip_prefix(&format!("`test_fails_{k}` ")).expect(message);
        let (did, rest) = rest.split_once("; the reports of seed").expect(message);
        let once = (1..=5).any(|seed| did == format!("failed under seed {seed} only"));
        assert!(once || did == "neither passed nor failed under any seed", "{message}");
        assert!(
            rest.ends_with(" have it skipped or not at all, so too few runs tell whether it is flaky."),
            "{message}"
        );
    }
}

#[test]
fn a_test_missing_from_a_run_s_report_is_not_counted_as_passed() {
    let repo = repo();
    repo.put("pytest.ini", b"[pytest]\naddopts = --maxfail=1\n");
    let text = four_failing();
    repo.put(STATE, text.as_bytes());
    assert_too_few_runs_judged(&findings(&repo), &text);
}

#[test]
fn the_project_s_pyproject_toml_configures_the_reruns() {
    // Without the table, each of the four would fail under every seed, and none would be judged too seldom.
    let tables =
        ["[tool.pytest.ini_options]\naddopts = \"--maxfail=1\"\n", "[tool.pytest]\naddopts = [\"--maxfail=1\"]\n"];
    for table in tables {
        let repo = repo();
        repo.put("pyproject.toml", format!("[project]\nname = 'p'\n\n[tool.ruff]\n\n{table}").as_bytes());
        let text = four_failing();
        repo.put(STATE, text.as_bytes());
        assert_too_few_runs_judged(&findings(&repo), &text);
    }
}

#[test]
fn a_test_skipped_under_some_seeds_is_judged_by_the_others() {
    // Skipped on the third run, failing on the second and fourth: the third is neither a pass nor a failure.
    let repo = repo();
    let text = alternating().replace(
        "            self.assertEqual(len(f.read()) % 2, 1)\n",
        "            runs = len(f.read())\n        if runs == 3:\n            self.skipTest('the third run')\n        \
         self.assertEqual(runs % 2, 1)\n",
    );
    repo.put(STATE, text.as_bytes());
    let messages: Vec<String> = findings(&repo).into_iter().map(|(.., message)| message).collect();
    assert_eq!(
        messages,
        [format!(
            "`TestRuns.test_alternates` failed under seeds 2 and 4 and passed under seeds 1 and 5, and was skipped or \
             not run under seed 3: {DEPENDS}"
        )]
    );
}

#[test]
fn a_test_that_only_lost_lines_is_rerun() {
    let repo = repo();
    repo.put(STATE, BASE.replace(&format!("        {SKIP}\n"), "").as_bytes());
    let found: Vec<(Option<u32>, String)> = findings(&repo).into_iter().map(|(_, line, _, m)| (line, m)).collect();
    assert_eq!(found, [(Some(15), alternates("TestRuns.test_alternates"))]);
}

#[test]
fn a_decorator_removed_above_the_others_reruns_its_test() {
    let repo = repo();
    let decorated = BASE.replace(&format!("        {SKIP}\n"), "").replace(
        "    def test_alternates(self):\n",
        "    @unittest.skip('not yet')\n    @unittest.expectedFailure\n    def test_alternates(self):\n",
    );
    repo.commit(&[(STATE, decorated.as_bytes())], "decorated");
    let bare = decorated.replace("    @unittest.skip('not yet')\n", "").replace("    @unittest.expectedFailure\n", "");
    repo.put(STATE, bare.as_bytes());
    assert_eq!(found(&repo), [at(STATE, 15, "mixed-results")]);
}

#[test]
fn a_test_file_whose_name_holds_a_line_end_is_rerun() {
    // A carriage return and U+2028 end a line for some readers of pytest's list of node ids, but not for pytest.
    let repo = repo();
    let mut old = STATE;
    for name in ["tests/test_st\u{2028}ate.py", "tests/test_st\rate.py"] {
        repo.git(&["mv", old, name]);
        repo.git(&["commit", "--quiet", "-m", "renamed"]);
        old = name;
        repo.put(name, alternating().as_bytes());
        assert_eq!(found(&repo), [at(name, 15, "mixed-results")]);
        repo.git(&["reset", "--quiet", "--hard"]);
    }
}

#[test]
fn a_test_file_whose_name_is_not_utf8_is_rerun_or_said_not_to_be() {
    use std::os::unix::ffi::OsStrExt;
    let repo = repo();
    let name = std::ffi::OsStr::from_bytes(b"tests/test_\xff.py");
    repo.git(&[std::ffi::OsStr::new("mv"), std::ffi::OsStr::new(STATE), name]);
    repo.git(&["commit", "--quiet", "-m", "renamed"]);
    repo.put(name, alternating().as_bytes());
    let found = findings(&repo);
    assert_eq!(found.len(), 1, "{found:?}");
    let (path, line, rule, message) = &found[0];
    if rule == "not-rerun" {
        // pytest collects and lists the test, but pytest-randomly 5.0.0 hashes each node id as strict UTF-8
        // (pytest_randomly._crc32), so the first rerun stops with an internal error, which the finding quotes.
        assert_eq!((path.as_deref(), *line), (None, None));
        let said = message.strip_prefix("pytest, rerunning them under seed 1, exited with status 3: ").expect(message);
        assert!(said.starts_with("UnicodeEncodeError: "), "{message}");
        assert!(said.ends_with("surrogates not allowed; the changed tests were not rerun."), "{message}");
    } else {
        assert_eq!((path.as_deref(), *line, rule.as_str()), (Some("tests/test_\\xff.py"), Some(15), "mixed-results"));
    }
}

#[test]
fn a_parameter_id_that_holds_colons_is_judged() {
    let repo = repo();
    let test = alternating_test("test_p(x)", "@pytest.mark.parametrize('x', ['a::b'])\n");
    repo.put("tests/test_p.py", test.as_bytes());
    assert_eq!(
        findings(&repo),
        [(Some("tests/test_p.py".into()), Some(9), "mixed-results".into(), alternates("test_p[a::b]"))]
    );
}

/// The findings on test_r, a test that alternates, on a change of the lines above its def from `old_above` to
/// `new_above`, one line, so that its def is on line 9.
fn decorated(old_above: &str, new_above: &str) -> Vec<Found> {
    let repo = repo();
    repo.commit(&[("tests/test_r.py", alternating_test("test_r()", old_above).as_bytes())], "test_r");
    repo.put("tests/test_r.py", alternating_test("test_r()", new_above).as_bytes());
    findings(&repo)
}

/// test_r's `mixed-results` finding, as [`decorated`] gives it.
fn test_r_is_mixed() -> Vec<Found> {
    vec![(Some("tests/test_r.py".into()), Some(9), "mixed-results".into(), alternates("test_r"))]
}

#[test]
fn a_decorator_replaced_by_a_comment_reruns_its_test() {
    let found = decorated("@pytest.mark.skip(reason='alternates, #12')\n", "# Was skipped for #12.\n");
    assert_eq!(found, test_r_is_mixed());
}

#[test]
fn a_decorator_removed_above_a_comment_reruns_its_test() {
    let comment = "# Alternates between runs.\n";
    let found = decorated(&format!("@pytest.mark.skip(reason='#12')\n{comment}"), comment);
    assert_eq!(found, test_r_is_mixed());
}

#[test]
fn a_test_that_xfails_under_every_seed_is_consistent() {
    let repo = repo();
    let old = "import pytest\n\n\n@pytest.mark.xfail(strict=True, reason='bug 12')\ndef test_a():\n    assert 1 == 2\n";
    repo.commit(&[("tests/test_x.py", old.as_bytes())], "xfail");
    repo.put("tests/test_x.py", old.replace("1 == 2", "1 == 3").as_bytes());
    assert_eq!(findings(&repo), vec![]);
}

#[test]
fn a_test_that_xfails_under_some_seeds_and_passes_under_others_is_mixed() {
    let found = decorated("", "@pytest.mark.xfail(reason='#12')\n");
    let message = format!(
        "`test_r` failed as its xfail mark expects under seeds 2 and 4 and passed under seeds 1, 3 and 5: {DEPENDS}"
    );
    assert_eq!(found, [(Some("tests/test_r.py".into()), Some(9), "mixed-results".into(), message)]);
}

#[test]
fn no_config_or_conftest_above_the_copy_is_loaded() {
    // Another account can write the directory the copy is made in; pytest must not take its pytest.ini, which would
    // make that directory confcutdir, nor so import its conftest.py.
    let repo = repo();
    let shared = repo.outside.join("shared");
    let pwned = shared.join("PWNED");
    std::fs::create_dir(&shared).unwrap();
    std::fs::write(shared.join("pytest.ini"), "[pytest]\n").unwrap();
    let conftest = format!("with open('{}', 'a') as f:\n    f.write('conftest ran\\n')\n", pwned.display());
    std::fs::write(shared.join("conftest.py"), conftest).unwrap();
    repo.put(STATE, alternating().as_bytes());
    let found = findings_with(&repo, &common::python(), &[("TMPDIR", shared.into())]);
    assert_eq!(found, [(Some(STATE.into()), Some(15), "mixed-results".into(), alternates("TestRuns.test_alternates"))]);
    assert!(!pwned.exists());
}

#[test]
fn a_configuration_below_the_top_configures_the_tests_under_it() {
    // tests/pytest.ini puts tests/src on the path, which the changed test needs to import; and the alternating test
    // shows it was rerun and judged.
    let repo = repo();
    repo.commit(
        &[("tests/pytest.ini", b"[pytest]\npythonpath = src\n"), ("tests/src/helper.py", b"VALUE = 1\n")],
        "nested config",
    );
    let text = format!("import helper\n{}", BASE.replace(SKIP, "self.assertEqual(helper.VALUE, 1)"));
    repo.put(STATE, text.as_bytes());
    assert_eq!(found(&repo), [at(STATE, 16, "mixed-results")]);
}

#[test]
fn tests_under_two_configurations_are_rerun_once_by_each() {
    let old = "def test_old():\n    assert True\n";
    let repo = Repo::with(&[
        ("pyproject.toml", b"[tool.ruff]\n"),
        ("a/pyproject.toml", b"[tool.pytest.ini_options]\n"),
        ("a/test_a.py", old.as_bytes()),
        ("b/tox.ini", b"[pytest]\n"),
        ("b/test_b.py", old.as_bytes()),
    ]);
    for path in ["a/test_a.py", "b/test_b.py"] {
        repo.put(path, format!("{old}\n\ndef test_new():\n    assert True\n").as_bytes());
    }
    let (python, log) = logging_python(&repo);
    assert_eq!(findings_with(&repo, &python, &[]), vec![]);
    // Each run's rootdir and configuration file, from the copy's top, and whether it was a collection.
    let mut runs = vec![];
    for line in std::fs::read_to_string(&log).unwrap().lines().filter(|line| reruns_or_collects(line)) {
        let words: Vec<&str> = line.split(' ').collect();
        let after = |flag: &str| words[words.iter().position(|word| *word == flag).unwrap() + 1];
        let top = format!("{}/", after("--confcutdir"));
        let in_copy = |path: &str| path.strip_prefix(&top).unwrap().to_string();
        runs.push((in_copy(after("--rootdir")), in_copy(after("-c")), line.contains("--collect-only")));
    }
    let by = |place: &str, inifile: &str, collects: bool| (place.to_string(), inifile.to_string(), collects);
    runs.sort();
    let (a, b) = runs.split_at(runs.len() / 2);
    assert_eq!(a, [vec![by("a", "a/pyproject.toml", false); 5], vec![by("a", "a/pyproject.toml", true)]].concat());
    assert_eq!(b, [vec![by("b", "b/tox.ini", false); 5], vec![by("b", "b/tox.ini", true)]].concat());
}

/// The findings on tests/test_g.py changed from `base` to `changed`, with the `untracked` files, (path, contents),
/// beside it.
fn failing_in_the_copy(base: &str, changed: &str, untracked: &[(&str, &str)]) -> Vec<Found> {
    let repo = repo();
    repo.commit(&[("tests/test_g.py", base.as_bytes())], "test_g");
    for (path, contents) in untracked {
        repo.put(path, contents.as_bytes());
    }
    repo.put("tests/test_g.py", changed.as_bytes());
    findings(&repo)
}

#[test]
fn a_test_that_needs_git_s_metadata_fails_every_run_in_the_copy() {
    let test = "import os\nimport subprocess\n\n\ndef test_head():\n    \
                subprocess.run(['git', 'rev-parse', 'HEAD'], check=True, cwd=os.path.dirname(__file__))\n";
    let found = failing_in_the_copy(&test.replace("check=True", "check=False"), test, &[]);
    let message = "`test_head` failed under seeds 1, 2, 3, 4 and 5, every rerun, which ran in a copy of the tracked \
                   files. If the build failed it too, the failing tests list it. If the build passed it, it needs \
                   something the copy lacks, such as git's metadata, an untracked or ignored file or a build product, \
                   so whether it is flaky could not be judged here.";
    assert_eq!(found, [(Some("tests/test_g.py".into()), Some(5), "failed-every-run".into(), message.into())]);
}

#[test]
fn a_test_that_needs_an_ignored_file_fails_every_run_in_the_copy() {
    let test = "import os\n\n\ndef test_settings():\n    \
                assert os.path.exists(os.path.join(os.path.dirname(__file__), '..', 'settings.local'))\n";
    let untracked = [(".gitignore", "settings.local\n"), ("settings.local", "x = 1\n")];
    let found = failing_in_the_copy(&test.replace("assert ", "assert True or "), test, &untracked);
    let places: Vec<(Option<u32>, &str)> = found.iter().map(|(_, line, rule, _)| (*line, rule.as_str())).collect();
    assert_eq!(places, [(Some(4), "failed-every-run")]);
}

/// The findings, (line, rule, message), on the change that adds `extra` to the test file, when the rerun under `seed`
/// goes wrong: Python exits with status 3 for it, as pytest does on an internal error.
fn stopped_at(seed: u32, extra: &str) -> Vec<(Option<u32>, String, String)> {
    let repo = repo();
    repo.put(STATE, format!("{BASE}{extra}").as_bytes());
    let script = format!(
        "for arg in \"$@\"; do [ \"$arg\" != --randomly-seed={seed} ] || {{ echo 'it broke' >&2; exit 3; }}; done"
    );
    let found = findings_with(&repo, &python_after(&repo, &script), &[]);
    found.into_iter().map(|(_, line, rule, message)| (line, rule, message)).collect()
}

#[test]
fn a_test_one_run_failed_before_the_next_went_wrong_is_not_failing_every_run() {
    let found = stopped_at(2, "\n\ndef test_one():\n    assert False\n");
    let expected = [
        (
            None,
            "not-rerun".to_string(),
            "pytest, rerunning them under seed 2, exited with status 3: it broke; only the reruns under seed 1 were \
             judged."
                .to_string(),
        ),
        (
            Some(23),
            "not-rerun".to_string(),
            "`test_one` failed under seed 1 only; the reruns stopped when the run under seed 2 went wrong, so too few \
             runs tell whether it is flaky."
                .to_string(),
        ),
    ];
    assert_eq!(found, expected);
}

#[test]
fn a_test_the_runs_before_a_stop_skipped_is_judged_by_too_few() {
    let extra = "\n\nimport pytest\n\n\ndef test_skipped():\n    pytest.skip('x')\n\n\n\
                 def test_fails():\n    assert False\n";
    let found = stopped_at(3, extra);
    let places: Vec<(Option<u32>, &str)> = found.iter().map(|(line, rule, _)| (*line, rule.as_str())).collect();
    assert_eq!(places, [(None, "not-rerun"), (Some(26), "not-rerun"), (Some(30), "failed-every-run")]);
    assert_eq!(
        found[0].2,
        "pytest, rerunning them under seed 3, exited with status 3: it broke; only the reruns under seeds 1 and 2 \
         were judged."
    );
    assert_eq!(
        found[1].2,
        "`test_skipped` neither passed nor failed under any seed; the reports of seeds 1 and 2 have it skipped or not \
         at all; the reruns stopped when the run under seed 3 went wrong, so too few runs tell whether it is flaky."
    );
}

/// Whether `line`, the arguments of a pytest run as [`logging_python`] logs them, is one of this check's: a rerun
/// under a seed, or its collection, which the passes-at-base check's, the gate's other one, tells from its own by
/// `--continue-on-collection-errors`.
fn reruns_or_collects(line: &str) -> bool {
    let collects = line.contains("--collect-only") && !line.contains("--continue-on-collection-errors");
    line.contains("randomly-seed") || collects
}

/// Whether the gate, judging `repo`'s change, ran pytest for the check, having found nothing.
fn reran(repo: &Repo) -> bool {
    let (python, log) = logging_python(repo);
    assert_eq!(findings_with(repo, &python, &[]), vec![]);
    std::fs::read_to_string(log).is_ok_and(|runs| runs.lines().any(reruns_or_collects))
}

#[test]
fn a_change_to_no_test_reruns_nothing() {
    let repo = repo();
    repo.put("calc.py", b"def add(a, b):\n    return b + a\n");
    repo.put(STATE, BASE.replace("import unittest\n", "import unittest  # edited\n").as_bytes());
    assert!(!reran(&repo));
    // And a change to a test does.
    repo.put(STATE, format!("{BASE}\n\ndef test_new():\n    assert True\n").as_bytes());
    assert!(reran(&repo));
}

#[test]
fn mutmut_s_copy_of_a_test_is_not_rerun() {
    // mutmut copies the tests into mutants/ beside the mutated code; committed, they are its state, not tests.
    let repo = repo();
    repo.put("mutants/tests/test_state.py", format!("{BASE}\n\ndef test_new():\n    assert True\n").as_bytes());
    assert!(!reran(&repo));
}

/// The one finding of `found`, which is on no path or line and of rule `not-rerun`: its message.
fn whole_not_rerun(found: Vec<Found>) -> String {
    assert_eq!(found.len(), 1, "{found:?}");
    let (path, line, rule, message) = found.into_iter().next().unwrap();
    assert_eq!((path, line, rule.as_str()), (None, None, "not-rerun"), "{message}");
    message
}

#[test]
fn a_rerun_that_cannot_start_is_a_finding() {
    let repo = repo();
    change_the_tests(&repo);
    let python = repo.outside.join("no-python");
    let message = whole_not_rerun(findings_with(&repo, &python, &[]));
    assert_eq!(
        message,
        format!(
            "could not start pytest with the Python `{}`, collecting the changed tests: No such file or directory \
             (os error 2); the changed tests were not rerun.",
            python.display()
        )
    );
}

#[test]
fn no_room_for_the_reruns_is_a_finding() {
    let repo = repo();
    change_the_tests(&repo);
    let missing = repo.outside.join("missing");
    let message = whole_not_rerun(findings_with(&repo, &common::python(), &[("TMPDIR", missing.into())]));
    assert_eq!(
        message,
        "could not make a directory to rerun the changed tests in: No such file or directory (os error 2); nothing \
         ran, so the check judged nothing."
    );
}

#[test]
fn the_reruns_leave_the_build_s_reports_alone() {
    // The project's addopts write a coverage report, as the build's own run did before the gate reads it.
    let repo = repo();
    repo.commit(&[("pytest.ini", b"[pytest]\naddopts = --cov=calc --cov-report=xml\n")], "coverage");
    repo.put("coverage.xml", b"the build's report\n");
    repo.put(STATE, alternating().as_bytes());
    assert_eq!(found(&repo), [at(STATE, 15, "mixed-results")]);
    assert_eq!(std::fs::read_to_string(repo.repo.join("coverage.xml")).unwrap(), "the build's report\n");
    let listed = |dir: &Path| -> Vec<String> {
        let names = std::fs::read_dir(dir).unwrap().map(|entry| entry.unwrap().file_name().into_string().unwrap());
        let mut names: Vec<String> = names.collect();
        names.sort();
        names
    };
    let top = [".git", "calc.py", "coverage.xml", "gate.toml", "pyproject.toml", "pytest.ini", "tests"];
    assert_eq!(listed(&repo.repo), top);
    assert_eq!(listed(&repo.repo.join("tests")), ["test_state.py"]);
}

#[test]
fn tracked_files_that_cannot_be_copied_are_a_finding() {
    let repo = repo();
    repo.make_uncopyable();
    change_the_tests(&repo);
    let said = whole_not_rerun(findings(&repo));
    let said = said.strip_prefix("could not copy the tracked files to rerun the changed tests in: cannot copy /");
    let said = said.expect("the copy's failure");
    let end = "/vendor/lib/pipe: it is no regular file, link or directory; the changed tests were not rerun.";
    assert!(said.ends_with(end), "{said}");
}

#[test]
fn a_rerun_directory_left_behind_is_a_finding_beside_the_results() {
    // A test leaves a directory in the copy that its owner cannot write, so what it holds cannot be removed. (Root
    // can remove it still: this test is for an account like the build's.)
    let repo = repo();
    repo.put(STATE, alternating().as_bytes());
    let locks = "import os\n\nLOCKED = os.path.join(os.path.dirname(__file__), 'locked')\n\n\ndef test_locks():\n    \
                 os.makedirs(LOCKED, exist_ok=True)\n    os.chmod(LOCKED, 0o700)\n    \
                 open(os.path.join(LOCKED, 'kept'), 'w').close()\n    os.chmod(LOCKED, 0o500)\n";
    repo.put("tests/test_locks.py", locks.as_bytes());
    let found = findings(&repo);
    let left: Vec<&Found> = found.iter().filter(|(_, _, rule, _)| rule == "not-removed").collect();
    assert_eq!(left.len(), 1, "{found:?}");
    let (path, line, _, message) = left[0];
    let named = message.strip_prefix("could not remove the directory to rerun the changed tests in, `").expect(message);
    let (dir, why) = named.split_once("`: ").expect(message);
    assert!(Command::new("chmod").args(["-R", "u+rwX", dir]).status().unwrap().success());
    std::fs::remove_dir_all(dir).unwrap();
    assert_eq!((path.as_deref(), *line), (None, None));
    assert!(why.contains("Permission denied"), "{message}");
    let mixed = (Some(STATE.into()), Some(15), "mixed-results".into(), alternates("TestRuns.test_alternates"));
    assert!(found.contains(&mixed), "{found:?}");
}

#[test]
fn a_test_file_that_does_not_collect_is_a_finding() {
    let repo = repo();
    let text = change_the_tests(&repo);
    repo.put(STATE, format!("{text}\nimport no_such_module\n").as_bytes());
    let message = whole_not_rerun(findings(&repo));
    assert!(message.starts_with("pytest, collecting the changed tests, exited with status 2: "), "{message}");
    assert!(message.ends_with("; the changed tests were not rerun."), "{message}");
}

#[test]
fn an_unparsable_test_file_is_a_finding() {
    let repo = repo();
    repo.put(STATE, format!("{BASE}def test_new(:\n").as_bytes());
    let found = findings(&repo);
    let places: Vec<_> = found.iter().map(|(path, line, rule, _)| (path.as_deref(), *line, rule.as_str())).collect();
    assert_eq!(places, [(Some(STATE), None, "not-rerun")]);
    assert!(found[0].3.ends_with(", so its tests were not rerun."), "{}", found[0].3);
}
