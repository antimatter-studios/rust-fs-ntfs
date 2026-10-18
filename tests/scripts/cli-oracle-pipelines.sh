#!/usr/bin/env bash
# No pipeline in scripts/cli-oracle.sh ends in a reader that stops early.
#
# cli-oracle.sh runs under `set -euo pipefail`. In `producer | head -n 100`
# head exits after its hundredth line; a producer still writing then dies of
# SIGPIPE, pipefail makes the pipeline's status 141, and `set -e` ends the
# step with it. Whether the producer is still writing depends on scheduling,
# so the step passes on one host and fails on another: the Slate capture of
# `cli-oracle.sh rename` died with exit 141 on the Ubuntu runner while the
# Windows matrix host passed it (#1064). A reader that consumes its whole
# input (`sed -n 1,100p`, `awk 'NR <= 100'`) gives the same lines and no
# signal.
#
# The early-closing readers refused: `head` and `grep -q`/`grep -m`. Reading
# a file directly (`head -c N /dev/urandom`) is not a pipeline and is fine.
#
#   bash tests/scripts/cli-oracle-pipelines.sh
set -uo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
script="$REPO/scripts/cli-oracle.sh"
fails=0

ok() { :; }
fail() { echo "FAIL  $*" >&2; fails=$((fails + 1)); }

# early FILE: every non-comment line of FILE that pipes into an
# early-closing reader, as LINE:TEXT.
early() {
    grep -nE '\|[[:space:]]*(head|grep[[:space:]]+(-[a-zA-Z]*[qm]|--quiet|--max-count))([[:space:]]|$)' "$1" |
        grep -vE '^[0-9]+:[[:space:]]*#'
}

# The scan is proved to see the pattern it refuses, so a clean result is
# not a scan that matches nothing.
sample="$(mktemp)"
trap 'rm -f "$sample"' EXIT
cat >"$sample" <<'EOF'
        awk '{ print }' "$f" | sort | cut -f2 | head -n 100 >"$out"
        printf '%s' "$x" |grep -q needle && echo found
        # a comment that mentions | head -n 1 is not code
        random() { head -c "$2" /dev/urandom >"$1"; }
        sed -n 1,100p "$f" >"$out"
EOF
hits="$(early "$sample" | cut -d: -f1 | tr '\n' ' ')"
if [ "$hits" = "1 2 " ]; then ok; else fail "the scan found lines '$hits' of the sample, not '1 2 '"; fi

[ -f "$script" ] || fail "$script is missing"
found="$(early "$script")"
if [ -z "$found" ]; then
    ok
else
    while IFS= read -r line; do
        fail "scripts/cli-oracle.sh:$line pipes into a reader that stops early; under pipefail its writer can die of SIGPIPE (exit 141)"
    done <<<"$found"
fi

[ "$fails" -eq 0 ] || exit 1
echo "cli-oracle-pipelines: all checks passed"
