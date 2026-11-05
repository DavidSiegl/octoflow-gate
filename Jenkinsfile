// The multibranch job Octoflow-AI/octoflow-gate: every build of a branch or pull request installs, tests gate.py with
// branch coverage and the gate-rust helper with line coverage, and judges the change with the gate itself, archiving
// quality.md. A pull request or main fails on GATE: ERROR, and on a gate that could not judge.
pipeline {
    // Agents wrote the code this builds, so it runs where they already do: the agent on their own box.
    // Never on the agents that deploy the homelab. Jenkins enforces that per node, not this label.
    agent { label 'untrusted' }

    options {
        skipDefaultCheckout()
        // Jenkins never discards the last successful build, which GIT_PREVIOUS_SUCCESSFUL_COMMIT names for the Gate
        // stage.
        buildDiscarder(logRotator(numToKeepStr: '30'))
    }

    environment {
        UV_NO_PROGRESS = '1'
        COVERAGE_FILE  = '.coverage'
        // Crates are fetched into the workspace, which cleanWs empties, so no build compiles sources an earlier build
        // left behind. gate.py notes an edit to the cargo config.toml there as build configuration.
        CARGO_HOME     = "${env.WORKSPACE}/.cargo-home"
        // Tools the Install stage downloads into the workspace (ci/install-tools.sh), first on PATH for every stage.
        PATH           = "${env.WORKSPACE}/.tools/bin:${env.PATH}"
        // Temporary files on the agent's mounted volume, beside the workspace, rather than in /tmp. The tests and the
        // gate build Rust in temporary directories and run what they build; /tmp is in the container's writable
        // layer, where the agent host's Falco reports every program run from a file written there ("Drop and execute
        // new binary in container"). Python's tempfile takes /tmp, silently, for a TMPDIR that does not exist, so the
        // Install stage creates it.
        TMPDIR         = "${env.WORKSPACE_TMP}/tmp"
    }

    stages {
        stage('Checkout') {
            steps {
                script {
                    def checkedOut = checkout scm
                    // The Gate stage measures a build of main from here.
                    env.PREVIOUS_SUCCESSFUL_COMMIT = checkedOut.GIT_PREVIOUS_SUCCESSFUL_COMMIT ?: ''
                }
            }
        }

        stage('Install') {
            steps {
                // What an earlier build of this workspace left in TMPDIR goes first: on the volume, unlike in the
                // container's /tmp, it would otherwise stay. The chmod is for a directory a killed test left without
                // write permission; a clear-out that still fails only warns, since failing here would fail every later
                // build of this workspace until someone cleaned the volume by hand.
                sh '''
                    if [ -e "$TMPDIR" ]; then
                        { chmod -R u+rwX "$TMPDIR" && rm -rf "$TMPDIR"; } ||
                            echo "could not empty $TMPDIR; what an earlier build left there stays" >&2
                    fi
                    mkdir -p "$TMPDIR"
                '''
                sh 'uv sync --frozen'
                // gate.py runs the real clippy, and cargo-llvm-cov reads its coverage data with llvm-tools'
                // llvm-profdata and llvm-cov, which must be the toolchain's own LLVM. Both are rustup components the
                // agent image provides; a build cannot add to its rustup home, which is root's. So a missing one is
                // named here rather than left to fail a later stage.
                sh '''
                    installed=$(rustup component list --installed)
                    for component in clippy llvm-tools; do
                        printf '%s\\n' "$installed" | grep -q "^$component-" || {
                            echo "rustup's $component component is not on the agent; its image must add it" >&2
                            exit 1
                        }
                    done
                '''
                sh 'ci/install-tools.sh "$WORKSPACE/.tools/bin"'
            }
        }

        stage('Test') {
            environment {
                // tests/conftest.py's larger profile: a build tries ten times the examples a local run does.
                HYPOTHESIS_PROFILE = 'ci'
            }
            steps {
                // A failing test does not stop the build here: the Gate stage judges it with everything else, so
                // quality.md lists failing tests and findings together. Any other exit status means the tests could
                // not run.
                //
                // gate-rust's own tests, with line coverage. --ignore-run-fail writes the coverage report whatever
                // tests fail and exits 0 for them, so a non-zero status is a build that did not compile or a tool
                // that could not run; it also passes a nextest that ran no test, so the JUnit report, which
                // ci/nextest.toml's ci profile writes, is removed first and must be there after.
                sh '''
                    junit=gate-rust/target/nextest/ci/junit.xml
                    rm -f "$junit"
                    cargo llvm-cov nextest --locked --manifest-path gate-rust/Cargo.toml --ignore-run-fail \\
                        --profile ci --config-file "$WORKSPACE/ci/nextest.toml" \\
                        --cobertura --output-path "$WORKSPACE/coverage-rust.xml"
                    if [ ! -f "$junit" ]; then
                        echo "cargo-nextest wrote no $junit, so gate-rust's tests did not run" >&2
                        exit 1
                    fi
                '''
                // --cov-context=test records which tests run each line, in COVERAGE_FILE, for the Gate stage's
                // --test-contexts: mutmut then runs only the tests that reach the changed functions. COVERAGE_FILE
                // stays relative, in the workspace: set to an absolute path elsewhere, the file vanished during a run
                // and over 2300 tests errored (octoflow-ai, where this stage began).
                sh '''
                    status=0
                    uv run --frozen pytest --cov=gate --cov-branch --cov-context=test --cov-report=xml \\
                        --junitxml=test-results.xml || status=$?
                    [ "$status" -le 1 ] || exit "$status"
                '''
            }
            post {
                always {
                    junit 'test-results.xml, gate-rust/target/nextest/ci/junit.xml'
                }
            }
        }

        // gate.py judges the change (docs/quality-gate.md) with main's gate, rules and gate-rust helper, so that a
        // change cannot loosen how it is itself judged. A pull request is measured from its merge base with its target,
        // a build of main from the last successful build's commit.
        stage('Gate') {
            steps {
                script {
                    def base
                    if (env.CHANGE_TARGET) {
                        base = sh(returnStdout: true, script: 'ci/pr-base.sh').trim()
                    } else {
                        // A history rewritten since the last successful build, by a force-push, leaves that build's
                        // commit out of this checkout, where gate.py cannot judge from it ("names no commit", exit
                        // status 2) in this and every later build; such a build measures from HEAD^1, as one with no
                        // successful build before it does.
                        def previous = env.PREVIOUS_SUCCESSFUL_COMMIT
                        def kept = previous && sh(returnStatus: true,
                                                  script: 'git cat-file -e "$PREVIOUS_SUCCESSFUL_COMMIT^{commit}"') == 0
                        base = kept ? previous : 'HEAD^1'
                    }
                    // main's gate.py, pyproject.toml and gate-rust, fetched without credentials, since the repository
                    // is public. A helper that does not build is one the gate cannot run, which each check that reads
                    // Rust reports as not run on a change to Rust, so it only warns here.
                    //
                    // Until gate-standalone/02 merges, main is the history filtered out of octoflow-ai alone: it has
                    // no pyproject.toml, so the change's own is taken, and keeps the helper in ci/gate-rust. Both
                    // fallbacks go once main has that change.
                    sh '''
                        git fetch --quiet origin +refs/heads/main:refs/remotes/origin/main
                        main="$WORKSPACE_TMP/main"
                        rm -rf "$main"
                        mkdir -p "$main"
                        git show origin/main:gate.py > "$main/gate.py"
                        if git cat-file -e origin/main:pyproject.toml 2>/dev/null; then
                            git show origin/main:pyproject.toml > "$main/pyproject.toml"
                        else
                            cp pyproject.toml "$main/pyproject.toml"
                        fi
                        for helper in gate-rust ci/gate-rust; do
                            if git cat-file -e "origin/main:$helper/Cargo.toml" 2>/dev/null; then
                                git archive origin/main "$helper" | tar -x -C "$main"
                                cargo build --release --locked --manifest-path "$main/$helper/Cargo.toml" \\
                                    --target-dir "$main/target" \\
                                    || echo "main's gate-rust helper did not build; gate.py reports Rust as not run" >&2
                                break
                            fi
                        done
                    '''
                    // The base reaches the shell as an environment variable, never as Groovy-built shell code.
                    def status
                    withEnv(["GATE_BASE=${base}"]) {
                        status = sh(returnStatus: true, script: '''
                            main="$WORKSPACE_TMP/main"
                            uv run --frozen python "$main/gate.py" --base "$GATE_BASE" --config "$main/pyproject.toml" \\
                                --tests gate-rust/target/nextest/ci/junit.xml --tests test-results.xml \\
                                --coverage coverage-rust.xml --coverage coverage.xml \\
                                --test-contexts "$COVERAGE_FILE" --rust-syntax "$main/target/release/gate-rust" \\
                                --out quality.md
                        ''')
                    }
                    if (status == 1) {
                        error('The quality gate did not pass; see quality.md')
                    } else if (status != 0) {
                        error("The quality gate could not judge the change (exit status ${status})")
                    }
                }
            }
            post {
                always {
                    archiveArtifacts artifacts: 'quality.md', allowEmptyArchive: true
                }
            }
        }
    }

    // Mail on a failure and on the first green build after one, never on a run that merely succeeded again.
    post {
        always {
            cleanWs()
        }
        failure {
            emailext(
                to: 'david.siegl95@gmail.com',
                subject: "FAILED  ${env.JOB_NAME} #${env.BUILD_NUMBER}",
                body: """Failed - see the attached log.

Gate report, if the build reached the Gate stage: ${env.BUILD_URL}artifact/quality.md
${env.BUILD_URL}console""",
                attachLog: true,
                compressLog: true
            )
        }
        fixed {
            emailext(
                to: 'david.siegl95@gmail.com',
                subject: "RECOVERED  ${env.JOB_NAME} #${env.BUILD_NUMBER}",
                body: "${env.BUILD_URL}"
            )
        }
    }
}
