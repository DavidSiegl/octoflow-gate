#!/bin/sh
# Exit 0 when a pull request's Jenkins build already judged the tree of HEAD, a commit on main, and everything
# since the last successful build of main, so that this build of main need not run the gate again; exit non-zero,
# saying why on standard error, when it must. The Jenkinsfile's "Judged at PR" stage runs it on builds of main.
# Modelled on octoflow-ai's ci/judged-at-pr.sh.
#
# Judged means: some pull request into main has a head with HEAD's tree that contains HEAD^1 and carries a
# successful pr-head status that $PR_STATUS_CREATOR posted, with HEAD^1 the commit of main's last successful build,
# $GIT_PREVIOUS_SUCCESSFUL_COMMIT. The same tree means that build tested HEAD's own code with HEAD's own tests; a
# head containing HEAD^1 means it judged every line since. It judged by main's rules as main stood when it ran
# (scripts/gate's header), which are HEAD^1's, the rules this build would judge by, when HEAD^1 came to main as a
# merge: a head containing that merge was built after main reached HEAD^1, and main stayed there until HEAD. A
# HEAD^1 pushed to main by a fast-forward may have been in the head before main had it, so the rules it brings need
# not have judged the head (TODO.md).
# Only Jenkins can post as $PR_STATUS_CREATOR, its own GitHub App: a status the agents post with their token never
# counts. The heads tried are those of the pull requests GitHub associates with HEAD whose base is main, the only
# ones built against main.
#
# Runs in the checkout, with gh logged in through GH_TOKEN. Any failure, such as an unset variable or a GitHub
# error, exits non-zero, so the build then judges HEAD itself.
set -eu

context=continuous-integration/jenkins/pr-head
if [ -z "${PR_STATUS_CREATOR:-}" ]; then
    echo "judged-at-pr.sh: PR_STATUS_CREATOR is empty, so no pull request build is trusted" >&2
    exit 1
fi
parent=$(git rev-parse HEAD^1)
tree=$(git rev-parse 'HEAD^{tree}')

if [ "$parent" != "${GIT_PREVIOUS_SUCCESSFUL_COMMIT:-}" ]; then
    echo "judged-at-pr.sh: the last successful build of main was not of HEAD^1 ($parent)" >&2
    exit 1
fi

heads=$(gh api "repos/{owner}/{repo}/commits/$(git rev-parse HEAD)/pulls" \
    --jq '.[] | select(.base.ref == "main") | .head.sha')
for head in $heads; do
    [ "$(gh api "repos/{owner}/{repo}/git/commits/$head" --jq .tree.sha)" = "$tree" ] || continue
    case "$(gh api "repos/{owner}/{repo}/compare/$parent...$head" --jq .status)" in
        identical | ahead) ;;
        *) continue ;;
    esac
    # Newest first; the newest of Jenkins's own, since Jenkins posts pending before the result.
    state=$(PR_STATUS_CONTEXT=$context gh api "repos/{owner}/{repo}/commits/$head/statuses?per_page=100" \
        --jq '[.[] | select(.context == env.PR_STATUS_CONTEXT and .creator.login == env.PR_STATUS_CREATOR)][0].state')
    if [ "$state" = success ]; then
        echo "judged-at-pr.sh: $head has HEAD's tree and passed its pull request build"
        exit 0
    fi
done
echo "judged-at-pr.sh: no pull request build by $PR_STATUS_CREATOR passed on HEAD's tree on top of $parent" >&2
exit 1
