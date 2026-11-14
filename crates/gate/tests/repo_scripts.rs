//! The two scripts through which this repository, and a project, runs a released gate: scripts/gate, which Jenkins
//! runs here, in how it picks BASE and the judge, fetches the gate and passes on its verdict; and ci/fetch-gate.sh,
//! in what it unpacks and what it refuses before unpacking. Both run offline: the release is a stub the test writes.

use std::path::{Path, PathBuf};
use std::process::Command;

use common::Repo;

mod common;

/// The path of `script`, relative to the workspace's root.
fn script(script: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").join(script)
}

/// A pin of the release `version`, whose sha256 no stub checks.
fn pin(version: &str) -> String {
    format!("{version} {}\n", "0".repeat(64))
}

/// A gate.toml that lists a test command, as scripts/gate asks of a judge.
const GATE_TOML: &str = "[[test]]\nname = \"t\"\ncommand = [\"true\"]\n";

/// A released gate, as [`fetching`] unpacks it. It refuses a gate.toml that sets `new_key`, as a gate refuses a key
/// it does not know, unless the pin it was fetched by names 0.8.0. Asked to write a quality file, it writes its
/// arguments, a line each, to GATE_ARGS, the gate.toml it judges by to GATE_SETTINGS and the CARGO_HOME it was given
/// to GATE_CARGO_HOME, and exits FAKE_GATE_EXIT.
const GATE_STUB: &str = r#"#!/bin/sh
args=$(printf '%s\n' "$@")
config= out=
while [ $# -gt 0 ]; do
    case $1 in
    --config) config=$2 ;;
    --out) out=$2 ;;
    esac
    shift
done
if grep -q '^new_key' "$config" && ! grep -q '^0\.8\.0 ' "$(dirname "$0")/pin"; then
    echo "error: unknown field new_key" >&2
    exit 2
fi
[ -n "$out" ] || exit 0
echo "$args" > "$GATE_ARGS"
cp "$config" "$GATE_SETTINGS"
printf '%s' "${CARGO_HOME-}" > "$GATE_CARGO_HOME"
exit "$FAKE_GATE_EXIT"
"#;

/// A ci/fetch-gate.sh that unpacks no download but [`GATE_STUB`], as the release's `gate`, and the pin it was given
/// beside it.
fn fetching() -> String {
    format!(
        "#!/bin/sh\nmkdir -p \"$2\"\ncp \"$1\" \"$2/pin\"\ncat > \"$2/gate\" <<'EOF'\n{GATE_STUB}EOF\n\
         chmod +x \"$2/gate\"\ncut -d' ' -f1 \"$1\"\n"
    )
}

/// A repository whose working tree holds scripts/gate, and whose branch main holds f.py alone.
fn script_repo() -> Repo {
    let repo = Repo::with(&[("f.py", b"x = 1\n")]);
    repo.put("scripts/gate", &std::fs::read(script("scripts/gate")).unwrap());
    repo
}

/// Commits to `repo` a judge that pins the release `version`, fetched by `fetch`, the text of its ci/fetch-gate.sh,
/// and judges by [`GATE_TOML`].
fn commit_judge(repo: &Repo, version: &str, fetch: &str) {
    use std::os::unix::fs::PermissionsExt;
    repo.put("ci/fetch-gate.sh", fetch.as_bytes());
    std::fs::set_permissions(repo.repo.join("ci/fetch-gate.sh"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let files = [("ci/gate.pin", pin(version)), ("pyproject.toml", String::new()), ("gate.toml", GATE_TOML.into())];
    let files: Vec<(&str, &[u8])> = files.iter().map(|(path, text)| (*path, text.as_bytes())).collect();
    repo.commit(&files, "a judge that pins a release");
}

/// What a run of scripts/gate left: its exit status and standard error, and what [`GATE_STUB`] wrote when it judged.
struct Judged {
    status: Option<i32>,
    stderr: String,
    /// The gate's arguments, None when it did not judge.
    gate_args: Option<Vec<String>>,
    /// The gate.toml the gate judged by, None when it did not judge.
    settings: Option<String>,
    /// The CARGO_HOME the gate ran with, empty when it was unset; None when it did not judge.
    cargo_home: Option<String>,
}

/// Runs `repo`'s scripts/gate as outside a Jenkins build, with stubs first on PATH for the tools it asks for before
/// it judges, uv among them, which runs what it is given, and the released gate exiting `gate_exit`.
fn run_gate(repo: &Repo, gate_exit: i32) -> Judged {
    run_gate_with(repo, gate_exit, &[])
}

/// [`run_gate`], with the environment variables `env`, (name, value), set beside.
fn run_gate_with(repo: &Repo, gate_exit: i32, env: &[(&str, &Path)]) -> Judged {
    let uv = "[ \"$1\" = sync ] && exit 0\nshift\nwhile [ \"${1#--}\" != \"$1\" ]; do shift; done\nexec \"$@\"";
    let mut bin = repo.stub("uv", uv);
    let installed = "echo clippy-x\necho llvm-tools-x";
    for (tool, script) in [("rustup", installed), ("cargo-llvm-cov", ""), ("cargo-nextest", ""), ("cargo-mutants", "")] {
        bin = repo.stub(tool, script);
    }
    let (gate_args, settings) = (repo.outside.join("gate.args"), repo.outside.join("gate.settings"));
    let cargo_home = repo.outside.join("gate.cargo-home");
    let done = Command::new("bash")
        .arg("scripts/gate")
        .current_dir(&repo.repo)
        .env("PATH", format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()))
        .envs([("GATE_ARGS", &gate_args), ("GATE_SETTINGS", &settings), ("GATE_CARGO_HOME", &cargo_home)])
        .env("FAKE_GATE_EXIT", gate_exit.to_string())
        .env_remove("WORKSPACE")
        .env_remove("WORKSPACE_TMP")
        .env_remove("CHANGE_TARGET")
        .env_remove("JENKINS_URL")
        .env_remove("CARGO_HOME")
        .envs(env.iter().copied())
        .output()
        .unwrap();
    Judged {
        status: done.status.code(),
        stderr: String::from_utf8(done.stderr).unwrap(),
        gate_args: std::fs::read_to_string(gate_args).ok().map(|args| args.lines().map(String::from).collect()),
        settings: std::fs::read_to_string(settings).ok(),
        cargo_home: std::fs::read_to_string(cargo_home).ok(),
    }
}

#[test]
fn the_default_base_is_the_branch_main_never_a_tag_main() {
    let repo = script_repo();
    repo.git(&["switch", "--quiet", "-c", "trunk"]);
    repo.git(&["branch", "--quiet", "-D", "main"]);
    repo.git(&["tag", "main"]);
    let judged = run_gate(&repo, 0);
    assert_eq!((judged.status, judged.stderr.as_str()), (Some(2), "gate: refs/heads/main is not a commit\n"));
}

#[test]
fn the_default_judge_is_the_branch_main_never_a_tag_main() {
    // The tag holds a pin the branch lacks: judged by the tag, the run would get past that check.
    let repo = script_repo();
    repo.git(&["switch", "--quiet", "-c", "trunk"]);
    repo.commit(&[("ci/gate.pin", pin("0.7.0").as_bytes())], "a judge");
    repo.git(&["tag", "main"]);
    let judged = run_gate(&repo, 0);
    let refused = "gate: refs/heads/main has no ci/gate.pin to judge with\n";
    assert_eq!((judged.status, judged.stderr.as_str()), (Some(2), refused));
}

#[test]
fn a_judge_without_test_commands_is_refused() {
    // Its gate would then run no test, and judge the change without them.
    let repo = script_repo();
    let judge: [(&str, &[u8]); 3] = [("ci/gate.pin", b""), ("ci/fetch-gate.sh", b""), ("pyproject.toml", b"")];
    repo.commit(&judge, "a judge without gate.toml");
    let judged = run_gate(&repo, 0);
    let refused = "gate: refs/heads/main has no gate.toml to judge with\n";
    assert_eq!((judged.status, judged.stderr.as_str()), (Some(2), refused));
    repo.commit(&[("gate.toml", b"[budgets]\nmutation = 600\n# [[test]]\n")], "a gate.toml without [[test]]");
    let judged = run_gate(&repo, 0);
    let refused = "gate: refs/heads/main's gate.toml lists no [[test]] command, so no test would be judged\n";
    assert_eq!((judged.status, judged.stderr.as_str()), (Some(2), refused));
}

#[test]
fn the_release_is_fetched_by_the_judge_s_script_not_the_change_s() {
    // A change that edits ci/fetch-gate.sh, say to skip the sha256 check, must not fetch the gate it is judged by.
    let repo = script_repo();
    commit_judge(&repo, "0.7.0", "#!/bin/sh\necho 'fetched by the judge' >&2\nexit 1\n");
    repo.put("ci/fetch-gate.sh", b"#!/bin/sh\necho 'fetched by the change' >&2\nexit 1\n");
    let judged = run_gate(&repo, 0);
    assert_eq!((judged.status, judged.stderr.as_str()), (Some(2), "fetched by the judge\n"));
}

#[test]
fn a_workspace_outside_a_jenkins_build_changes_nothing() {
    let repo = script_repo();
    commit_judge(&repo, "0.7.0", &fetching());
    let (workspace, workspace_tmp) = (repo.outside.join("workspace"), repo.outside.join("workspace@tmp"));
    let judged = run_gate_with(&repo, 0, &[("WORKSPACE", &workspace)]);
    assert_eq!(judged.status, Some(0), "{}", judged.stderr);
    // A Jenkins build empties $WORKSPACE_TMP/tmp first; outside one, it is someone else's to keep.
    let kept = workspace_tmp.join("tmp/kept");
    std::fs::create_dir_all(kept.parent().unwrap()).unwrap();
    std::fs::write(&kept, b"").unwrap();
    let judged = run_gate_with(&repo, 0, &[("WORKSPACE", &workspace), ("WORKSPACE_TMP", &workspace_tmp)]);
    assert_eq!(judged.status, Some(0), "{}", judged.stderr);
    assert!(kept.exists());
    assert_eq!(judged.cargo_home.as_deref(), Some(""));
}

#[test]
fn a_jenkins_build_keeps_its_temporary_files_in_its_workspace() {
    let repo = script_repo();
    commit_judge(&repo, "0.7.0", &fetching());
    let (workspace, workspace_tmp) = (repo.outside.join("workspace"), repo.outside.join("workspace@tmp"));
    let jenkins = Path::new("https://jenkins.example.com/");
    let judged = run_gate_with(&repo, 0, &[("JENKINS_URL", jenkins), ("WORKSPACE", &workspace)]);
    assert_eq!(judged.status, Some(2), "{}", judged.stderr);
    assert!(judged.stderr.contains("WORKSPACE_TMP is not"), "{}", judged.stderr);
    let build = [("JENKINS_URL", jenkins), ("WORKSPACE", &workspace), ("WORKSPACE_TMP", &workspace_tmp)];
    let judged = run_gate_with(&repo, 0, &build);
    assert_eq!(judged.status, Some(0), "{}", judged.stderr);
    assert!(workspace_tmp.join("tmp").is_dir());
    assert_eq!(judged.cargo_home, Some(workspace.join(".cargo-home").display().to_string()));
}

#[test]
fn the_gate_judges_by_the_judge_s_settings_and_its_verdict_is_the_script_s() {
    let repo = script_repo();
    commit_judge(&repo, "0.7.0", &fetching());
    repo.put("gate.toml", format!("# The change's own.\n{GATE_TOML}").as_bytes());
    let judged = run_gate(&repo, 1);
    assert_eq!(judged.status, Some(1), "{}", judged.stderr);
    assert!(judged.stderr.starts_with("gate: judging with octoflow-gate 0.7.0, the release refs/heads/main's"));
    assert_eq!(judged.settings.as_deref(), Some(GATE_TOML));
    let args = judged.gate_args.unwrap();
    let base = repo.git(&["rev-parse", "main"]);
    assert!(args[3].ends_with("/gate.toml") && !Path::new(&args[3]).starts_with(&repo.repo), "{}", args[3]);
    assert_eq!(args, ["--base", base.trim(), "--config", &args[3], "--python", "python", "--out", "quality.md"]);
    // Ctrl-C is the gate's to report; a status that is no verdict, a crash's included, is a change not judged.
    for (gate_exit, status) in [(0, 0), (130, 130), (2, 2), (101, 2)] {
        assert_eq!(run_gate(&repo, gate_exit).status, Some(status), "the gate exited {gate_exit}");
    }
}

#[test]
fn a_change_whose_gate_toml_its_pinned_release_refuses_is_not_judged() {
    // Merged, it would be read by that release's gate, which refuses a key it does not know: every later change, the
    // one raising the pin included, would then go unjudged.
    let repo = script_repo();
    commit_judge(&repo, "0.7.0", &fetching());
    repo.put("gate.toml", format!("new_key = 1\n{GATE_TOML}").as_bytes());
    let judged = run_gate(&repo, 0);
    assert_eq!(judged.status, Some(2));
    let refused = "error: unknown field new_key\n\
                   gate: the release ci/gate.pin pins could not judge by this gate.toml, for what it said above; \
                   one it refuses needs the pin raised first\n";
    assert!(judged.stderr.ends_with(refused), "{}", judged.stderr);
    assert_eq!(judged.gate_args, None);
}

#[test]
fn a_change_that_raises_the_pin_may_use_what_the_new_release_reads() {
    let repo = script_repo();
    commit_judge(&repo, "0.7.0", &fetching());
    repo.put("gate.toml", format!("new_key = 1\n{GATE_TOML}").as_bytes());
    repo.put("ci/gate.pin", pin("0.8.0").as_bytes());
    let judged = run_gate(&repo, 0);
    assert_eq!(judged.status, Some(0), "{}", judged.stderr);
    // Judged by the release the judge pins, with the judge's settings.
    assert!(judged.stderr.starts_with("gate: judging with octoflow-gate 0.7.0, "), "{}", judged.stderr);
    assert_eq!(judged.settings.as_deref(), Some(GATE_TOML));
}

/// A ci/fetch-gate.sh that unpacks as [`fetching`] does, except for a pin of 0.6.0, the last release before the
/// cutover: that one unpacks a gate named for its platform, `gate-linux-x86_64`, and no `gate`.
fn fetching_0_6_0_too() -> String {
    let rest = fetching().strip_prefix("#!/bin/sh\n").unwrap().to_string();
    format!(
        "#!/bin/sh\nif grep -q '^0\\.6\\.0 ' \"$1\"; then\n    mkdir -p \"$2\" && : > \"$2/gate-linux-x86_64\"\n    \
         echo 0.6.0\n    exit 0\nfi\n{rest}"
    )
}

#[test]
fn a_change_that_pins_a_release_without_gate_is_not_judged_and_told_why() {
    // Lowering the pin below 0.7.0, the first release that ships `gate`, is a rollback this script no longer runs.
    let repo = script_repo();
    commit_judge(&repo, "0.7.0", &fetching_0_6_0_too());
    repo.put("gate.toml", GATE_TOML.as_bytes());
    repo.put("ci/gate.pin", pin("0.6.0").as_bytes());
    let judged = run_gate(&repo, 0);
    assert_eq!(judged.status, Some(2));
    let refused = "gate: octoflow-gate 0.6.0, the release the change's ci/gate.pin pins, ships no gate; \
                   0.7.0 is the first that does\n";
    assert!(judged.stderr.ends_with(refused), "{}", judged.stderr);
    assert_eq!(judged.gate_args, None);
}

#[test]
fn a_judge_that_pins_a_release_without_gate_is_refused_before_anything_is_installed() {
    let repo = script_repo();
    commit_judge(&repo, "0.6.0", &fetching_0_6_0_too());
    let judged = run_gate(&repo, 0);
    let refused = "gate: octoflow-gate 0.6.0, the release refs/heads/main's ci/gate.pin pins, ships no gate; \
                   0.7.0 is the first that does\n";
    assert_eq!((judged.status, judged.stderr.as_str()), (Some(2), refused));
}

/// ci/fetch-gate.sh's world: a tarball to serve, and a curl that serves it.
struct Release {
    repo: Repo,
    /// The sha256 of the tarball, which holds one file, `gate`.
    sha256: String,
    /// Whether curl finds the tarball; false has it fail as `curl -f` does on an HTTP error.
    served: bool,
}

/// What a run of ci/fetch-gate.sh left.
struct Fetched {
    status: Option<i32>,
    stdout: String,
    stderr: String,
    /// The URLs curl was asked for, in order.
    urls: Vec<String>,
    /// The file `gate` it unpacked, None when it unpacked nothing.
    gate: Option<String>,
}

impl Release {
    fn new() -> Release {
        // A repository only for its directory outside, which is removed with it.
        let repo = Repo::with(&[("f", b"")]);
        let made = repo.outside.join("made");
        std::fs::create_dir_all(&made).unwrap();
        std::fs::write(made.join("gate"), "the released gate\n").unwrap();
        let tarball = repo.outside.join("release.tar.gz");
        let tar = Command::new("tar").arg("-czf").arg(&tarball).arg("-C").arg(&made).arg("gate").status().unwrap();
        assert!(tar.success());
        let summed = Command::new("sha256sum").arg(&tarball).output().unwrap();
        assert!(summed.status.success());
        let sha256 = String::from_utf8(summed.stdout).unwrap()[..64].to_string();
        Release { repo, sha256, served: true }
    }

    /// Runs fetch-gate.sh on a pin holding `pin_text`, or on a pin that is missing when None, into a directory that
    /// is not there yet.
    fn fetch(&self, pin_text: Option<&str>) -> Fetched {
        // Copies FAKE_TARBALL to the file after -o and appends the URL, its last argument, to CURL_LOG; exits 22
        // when FAKE_TARBALL is unset.
        let curl = "for arg; do last=$arg; done\necho \"$last\" >> \"$CURL_LOG\"\n\
                    [ -n \"${FAKE_TARBALL:-}\" ] || exit 22\n\
                    while [ $# -gt 0 ]; do\n    if [ \"$1\" = -o ]; then cp \"$FAKE_TARBALL\" \"$2\"; exit 0; fi\n    \
                    shift\ndone\nexit 2";
        let bin = self.repo.stub("curl", curl);
        let outside = &self.repo.outside;
        let (pin, out, log) = (outside.join("gate.pin"), outside.join("out"), outside.join("curl.log"));
        for left in [&pin, &log] {
            // Missing on the first fetch.
            let _ = std::fs::remove_file(left);
        }
        if let Some(pin_text) = pin_text {
            std::fs::write(&pin, pin_text).unwrap();
        }
        let mut fetch = Command::new("sh");
        fetch.arg(script("ci/fetch-gate.sh")).arg(&pin).arg(&out);
        fetch.env("PATH", format!("{}:{}", bin.display(), std::env::var("PATH").unwrap())).env("CURL_LOG", &log);
        if self.served {
            fetch.env("FAKE_TARBALL", outside.join("release.tar.gz"));
        }
        let done = fetch.output().unwrap();
        Fetched {
            status: done.status.code(),
            stdout: String::from_utf8(done.stdout).unwrap(),
            stderr: String::from_utf8(done.stderr).unwrap(),
            urls: std::fs::read_to_string(log).map(|log| log.lines().map(String::from).collect()).unwrap_or_default(),
            gate: std::fs::read_to_string(out.join("gate")).ok(),
        }
    }
}

#[test]
fn a_matching_release_is_unpacked_and_its_version_printed() {
    let release = Release::new();
    let fetched = release.fetch(Some(&format!("# a comment\n0.7.0 {}\n", release.sha256)));
    assert_eq!((fetched.status, fetched.stdout.as_str()), (Some(0), "0.7.0\n"), "{}", fetched.stderr);
    let url = "https://github.com/DavidSiegl/octoflow-gate/releases/download/v0.7.0/octoflow-gate-0.7.0.tar.gz";
    assert_eq!(fetched.urls, [url]);
    assert_eq!(fetched.gate.as_deref(), Some("the released gate\n"));
}

#[test]
fn a_tarball_that_does_not_match_the_sha256_unpacks_nothing() {
    let fetched = Release::new().fetch(Some(&pin("0.7.0")));
    assert_eq!(fetched.status, Some(1));
    assert!(fetched.stderr.contains("does not match the sha256"), "{}", fetched.stderr);
    assert_eq!(fetched.gate, None);
}

#[test]
fn a_failed_download_fails() {
    let release = Release { served: false, ..Release::new() };
    let fetched = release.fetch(Some(&format!("0.7.0 {}\n", release.sha256)));
    assert_eq!(fetched.status, Some(1));
    assert!(fetched.stderr.contains("cannot download"), "{}", fetched.stderr);
    assert_eq!(fetched.gate, None);
}

#[test]
fn a_malformed_pin_is_refused_before_any_download() {
    let release = Release::new();
    let sha256 = &release.sha256;
    for pin_text in [
        "# only a comment\n".to_string(),
        "0.7.0\n".to_string(),
        format!("0.7.0 {}\n", &sha256[..63]),
        format!("0.7.0 {}\n", sha256.to_uppercase()),
        format!("../0.7.0 {sha256}\n"),
        format!("0.7.0?x {sha256}\n"),
    ] {
        let fetched = release.fetch(Some(&pin_text));
        assert_eq!(fetched.status, Some(1), "{pin_text}");
        assert!(fetched.stderr.contains("is not '<version> <sha256>'"), "{pin_text}: {}", fetched.stderr);
        assert_eq!(fetched.urls, [""; 0], "{pin_text}");
    }
}

#[test]
fn an_unreadable_pin_is_refused() {
    let fetched = Release::new().fetch(None);
    assert_eq!(fetched.status, Some(1));
    assert!(fetched.stderr.contains("cannot read the pin"), "{}", fetched.stderr);
    assert_eq!(fetched.urls, [""; 0]);
}
