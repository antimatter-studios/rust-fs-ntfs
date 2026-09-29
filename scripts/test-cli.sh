#!/usr/bin/env bash
# test-cli.sh — THE `cli` TIER: the command-line tools as installed, found
# on PATH, not a cargo target.
#
# The same suite checks a checkout's build (`chore cli:install` stages it and
# prints the PATH line) and a Homebrew install (`brew install
# antimatter-studios/tap/rust-fs-ntfs`): it tests whatever PATH resolves,
# because that is what a user runs.
#
# STEP 1, BEFORE ANY TOOL IS TESTED: the tools are present and are ours.
#   - jq and rust-fs-ntfs must resolve, and rust-fs-ntfs must answer
#     --version as this crate;
#   - `rust-fs-ntfs doctor` must pass: every dotted name on PATH is our
#     program at our version. If one is shadowed -- a distribution's
#     /usr/sbin/mkfs.ntfs, another formula's, an older install of ours --
#     the tier fails with doctor's report: what wins, and the fix.
# NOTHING SKIPS. A missing tool fails the tier naming what provides it; a
# tier that tested someone else's mkfs.ntfs and reported green would be
# worse than no tier.
#
# STEP 2: every tests/cli/test-*.sh, by glob, so a new one needs no edit
# here. Each prints its failures, a `test result: ok. N passed; ...` line,
# and LAST `<name>: all checks passed`. A file that exits 0 without that
# last line stopped early and is a failure: `exit 0` part-way through is not
# evidence a file finished.
#
# STEP 3, THE FLOOR: the passes are summed, and a run that executed fewer
# checks than CLI_FLOOR fails even when every file passed. A file that
# stopped selecting its checks, or a glob that stopped matching one, would
# otherwise read as green. The number is measured, and moves up with the
# suite -- never down to make a run pass. See `test:cli` in chores.yml.
#
# Quiet: the tier runs under scripts/tier.sh, which keeps the whole run in
# tmp/logs/cli.log.
#
# CLI_TESTS names another directory of test-*.sh files, and CLI_FLOOR
# another floor, for tests/scripts/cli-tier.sh, which proves every refusal
# above refuses.
set -uo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
TESTS="${CLI_TESTS:-$REPO/tests/cli}"
# Measured: see `test:cli` in chores.yml for the count this is taken from.
FLOOR="${CLI_FLOOR:-82}"
CRATE="$(sed -n 's/^name *= *"\(.*\)"/\1/p' "$REPO/Cargo.toml" | head -n 1)"
INSTALL="build and stage it with \`chore cli:install\` (it prints the PATH line to use), or install it with \`brew install antimatter-studios/tap/rust-fs-ntfs\`"

refuse() {
    echo "test-cli: $*" >&2
    exit 1
}

command -v jq >/dev/null 2>&1 ||
    refuse "jq is not on PATH; the suite reads every JSON report with it. Install it: \`brew install jq\`, or \`apt-get install jq\`."

entry="$(command -v rust-fs-ntfs 2>/dev/null || true)"
[ -n "$entry" ] || refuse "rust-fs-ntfs is not on PATH: $INSTALL."
answer="$(rust-fs-ntfs --version 2>/dev/null || true)"
case "$answer" in
    "rust-fs-ntfs ($CRATE) "*) ;;
    *) refuse "$entry is not $CRATE's rust-fs-ntfs: --version answered '$answer'. Put ours first on PATH: $INSTALL." ;;
esac
echo "== $answer, at $entry"

echo "== rust-fs-ntfs doctor"
if ! report="$(rust-fs-ntfs doctor --text 2>&1)"; then
    printf '%s\n' "$report" >&2
    refuse "doctor found a tool on PATH that is not this program; its fixes are above. Nothing was tested."
fi
printf '%s\n' "$report"

shopt -s nullglob
files=("$TESTS"/test-*.sh)
[ "${#files[@]}" -gt 0 ] || refuse "no test-*.sh in $TESTS to run"

failed=0
executed=0
for file in "${files[@]}"; do
    name="$(basename "$file" .sh)"
    echo "== $name"
    output="$(bash "$file" 2>&1)"
    status=$?
    printf '%s\n' "$output"
    last="$(printf '%s\n' "$output" | tail -n 1)"
    n="$(printf '%s\n' "$output" | awk '/^test result: ok\./ { sum += $4 } END { print sum + 0 }')"
    executed=$((executed + n))
    if [ "$status" -ne 0 ]; then
        echo "FAIL  $name exited $status" >&2
        failed=$((failed + 1))
    elif [ "$last" != "$name: all checks passed" ]; then
        echo "FAIL  $name exited 0 without its last line, '$name: all checks passed': it stopped early" >&2
        failed=$((failed + 1))
    fi
done

if [ "$failed" -gt 0 ]; then
    echo "test-cli: $failed of ${#files[@]} files failed" >&2
    exit 1
fi
if [ "$executed" -lt "$FLOOR" ]; then
    echo "test-cli: only $executed checks executed, and the floor is $FLOOR: every file passed, so something stopped checks from running rather than failing them" >&2
    exit 1
fi
echo "test-cli: ${#files[@]} files, $executed checks (floor $FLOOR), all checks passed"
