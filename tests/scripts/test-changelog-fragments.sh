#!/usr/bin/env bash
# rust-fs-core's scripts/changelog-check.sh, run in place from the
# rust-fs-core checkout ($FS_CORE_ROOT, else ../rust-fs-core), must pass this
# repository's changelog.d/ fragments and REFUSE what breaks the agent-core v6
# rule: an unreleased entry written straight into CHANGELOG.md, and a fragment
# a release could not gather. A gate that cannot fail is no gate.
set -uo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$REPO" || exit 1

fails=0
ok()  { printf '  ok   %s\n' "$1"; }
bad() { printf '  FAIL %s\n' "$1" >&2; fails=$((fails + 1)); }

# The check is rust-fs-core's, and this repository keeps no copy of it. No
# checkout to run it from is a failure, not a skip.
CHECK="${FS_CORE_ROOT:-$REPO/../rust-fs-core}/scripts/changelog-check.sh"
if [ ! -f "$CHECK" ]; then
    bad "$CHECK does not exist: check rust-fs-core out beside this repository, or set FS_CORE_ROOT"
    exit 1
fi

BAK="$(mktemp)"; cp CHANGELOG.md "$BAK"
PROBE="changelog.d/zz-changelog-check-probe.md"
restore() { cp "$BAK" CHANGELOG.md; rm -f "$PROBE"; }
trap 'restore; rm -f "$BAK"' EXIT

# Without a pipe, so $? is the checker's own status.
expect() {
    local what="$1" want="$2"
    FS_CORE_CALLER="$REPO" bash "$CHECK" >/dev/null 2>&1
    local rc=$?
    case "$want" in
        zero)    [ "$rc" -eq 0 ] && ok "$what" || bad "$what: expected exit 0, got $rc" ;;
        nonzero) [ "$rc" -ne 0 ] && ok "$what" || bad "$what: expected non-zero, got $rc" ;;
    esac
}

expect "the committed changelog.d/ and CHANGELOG.md pass" zero

# An entry written under Unreleased, through awk (sed -i differs by platform).
awk '!done && /^##[[:space:]]+\[?[Uu]nreleased/ { print; print ""; print "### Fixed"; print ""; print "- **Written straight in.**"; done = 1; next } 1' \
    "$BAK" > CHANGELOG.md
expect "an entry written into CHANGELOG.md is refused" nonzero
restore

printf 'Words before any heading.\n' > "$PROBE"
expect "a fragment with text before its first heading is refused" nonzero
restore

expect "the tree is intact again afterwards" zero

[ "$fails" -eq 0 ] || exit 1
echo "changelog-fragments: all checks passed"
