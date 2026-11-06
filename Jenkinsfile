// Builds main and every pull request in a Multibranch Pipeline job and runs the quality
// gate (scripts/gate) on each, failing the build on GATE: ERROR or when the gate could
// not judge. For a pull request, Jenkins's GitHub App reports the result as the commit
// status continuous-integration/jenkins/pr-head, which auto-merge trusts only from that
// App, as does the ci-cd ruleset on main once agent-estate's init/github-rules.py has set
// it.
// docs/agents/README.md says how a pull request then merges; the agent-estate README,
// "Jenkins", says what the job and the agent need.
pipeline {
    // Builds run code agents wrote: only on agents that hold nothing an agent should not
    // have. Jenkins enforces that per node (Job Restrictions plugin), not this label.
    agent { label 'untrusted' }

    options {
        buildDiscarder(logRotator(numToKeepStr: '30'))
        timeout(time: 60, unit: 'MINUTES')
        // One build per branch at a time, so a build's previous one has always finished.
        // The mail on a pass after a failure then compares each build with the one before
        // it (post).
        disableConcurrentBuilds()
    }

    stages {
        // The change this build judges, from GATE_BASE to HEAD; scripts/gate decides how much
        // of it to run.
        stage('Change') {
            steps {
                script {
                    def base
                    if (env.CHANGE_TARGET) {
                        // A pull request: everything since it left its target branch, read
                        // by its full ref, since a tag or branch named origin/<target> would
                        // win over the short name (gitrevisions(7)).
                        base = sh(returnStdout: true, script: '''
                            git fetch --quiet origin "+refs/heads/$CHANGE_TARGET:refs/remotes/origin/$CHANGE_TARGET"
                            git merge-base HEAD "refs/remotes/origin/$CHANGE_TARGET"
                        ''').trim()
                    } else {
                        // A branch build: since the last successful build, or the last commit when
                        // there is none or a force-push removed it, or (a root commit) since a
                        // commit of the empty tree, made here and never pushed.
                        def previous = env.GIT_PREVIOUS_SUCCESSFUL_COMMIT
                        def kept = previous && sh(returnStatus: true,
                                                  script: 'git cat-file -e "$GIT_PREVIOUS_SUCCESSFUL_COMMIT^{commit}"') == 0
                        def parent = sh(returnStatus: true, script: 'git cat-file -e "HEAD^1^{commit}"') == 0
                        base = kept ? previous : parent ? 'HEAD^1' : sh(returnStdout: true, script: '''
                            empty=$(git hash-object -w -t tree /dev/null)
                            git -c user.name=gate -c user.email=gate@localhost -c commit.gpgSign=false \\
                                commit-tree "$empty" -m empty </dev/null
                        ''').trim()
                    }
                    // The base reaches the shell as an environment variable, never as Groovy-built
                    // shell code.
                    env.GATE_BASE = base
                }
            }
        }

        stage('Gate') {
            steps {
                script {
                    def status = sh(returnStatus: true, script: 'scripts/gate "$GATE_BASE"')
                    if (status == 1) {
                        error('GATE: ERROR - see quality.md')
                    } else if (status != 0) {
                        error("the gate could not judge the change (exit status ${status})")
                    }
                }
            }
        }
    }

    post {
        always {
            archiveArtifacts artifacts: 'quality.md, quality.json', allowEmptyArchive: true
            // The JUnit reports of the tests scripts/gate ran, which it leaves at the repo root,
            // shown as the build's test results and never judged: neither the build's result nor
            // a GitHub check (skipPublishingChecks) comes from them, so a failing test leaves the
            // result to the gate. None, as on a change to docs alone, is no error.
            junit allowEmptyResults: true, skipMarkingBuildUnstable: true, skipPublishingChecks: true,
                  testResults: 'test-results*.xml'
        }
        // Mail on every build that did not pass (failed, unstable or aborted), and on the first
        // passing build after one, never on a build that merely passed again.
        unsuccessful {
            emailext(
                to: 'david.siegl95@gmail.com',
                subject: "${currentBuild.currentResult}  ${env.JOB_NAME} #${env.BUILD_NUMBER}",
                body: """Did not pass - see the attached log.

Gate report, if the build reached the Gate stage: ${env.BUILD_URL}artifact/quality.md
${env.BUILD_URL}console""",
                attachLog: true,
                compressLog: true
            )
        }
        success {
            script {
                def previous = currentBuild.previousCompletedBuild
                if (previous && previous.result != 'SUCCESS') {
                    emailext(
                        to: 'david.siegl95@gmail.com',
                        subject: "RECOVERED  ${env.JOB_NAME} #${env.BUILD_NUMBER}",
                        body: "${env.BUILD_URL}"
                    )
                }
            }
        }
        // Last, whatever the steps above did, so no build leaves the workspace to the next.
        cleanup {
            cleanWs()
        }
    }
}
