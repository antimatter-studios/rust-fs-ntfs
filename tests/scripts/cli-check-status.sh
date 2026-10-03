#!/usr/bin/env bash
# No CLI check reads `$?` after a command substitution on the same line.
#
#   fs.ntfs "$img" mkdir /d 2>"$SANDBOX/mkdir.err"
#   check "mkdir /d exits 0 ($(cat "$SANDBOX/mkdir.err"))" test $? -eq 0
#
# Bash expands a command's words left to right, so the `$(cat ...)` in the
# message runs first and sets `$?` to cat's status, 0, before `test $?` is
# expanded. The check tests cat, not the tool, and can never fail (#416).
# The status has to be taken on the line after the command:
#
#   status=$?
#   check "mkdir /d exits 0 ($(cat "$SANDBOX/mkdir.err"))" test "$status" -eq 0
#
# Scanned by line over tests/cli/ and tests/cli-oracle/: a `check` line
# holding `$(` or a backtick before `$?`. Arithmetic `$((` runs no command
# and leaves `$?` alone, so it is not a hit. Comments are not code.
#
#   bash tests/scripts/cli-check-status.sh
set -uo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

fail() { echo "FAIL  $*" >&2; exit 1; }

# Every offending line in the files named, as FILE:LINE: TEXT; nothing when clean.
scan() {
    awk '
        /^[[:space:]]*#/ { next }
        /^[[:space:]]*check[[:space:]]/ {
            line = $0
            gsub(/\$\(\(/, "", line)          # arithmetic: runs no command
            q = index(line, "$?")
            if (q == 0) next
            before = substr(line, 1, q - 1)
            if (index(before, "$(") || index(before, "`"))
                print FILENAME ":" FNR ": " $0
        }
    ' "$@"
}

SANDBOX="$(mktemp -d)"
trap 'rm -rf "$SANDBOX"' EXIT HUP INT TERM

# --- 1. The scan refuses what it exists to refuse. ------------------------
#
# Without this, a scan that matched nothing would pass the real suites
# having checked nothing.
cat >"$SANDBOX/bad.sh" <<'EOF'
check "mkdir exits 0 ($(cat "$SANDBOX/mkdir.err"))" test $? -eq 0
    check "fsck exits 0 ($(head -c 600 out.json))" test $? -eq 0
check "write exits 0 (`cat w.err`)" test $? -eq 0
EOF
cat >"$SANDBOX/good.sh" <<'EOF'
# check "a comment ($(cat x))" test $? -eq 0
check "mkdir exits 0 ($(cat "$SANDBOX/mkdir.err"))" test "$status" -eq 0
check "a missing image exits 1" test $? -eq 1
check "the $((n + 1))th write exits 0" test $? -eq 0
check "exits 0 (exit $?)" test "$(cat x)" = y
status="$(cat x)"; rc=$?
EOF

found="$(scan "$SANDBOX/bad.sh")" || fail "the scan itself failed"
count="$(grep -c . <<<"$found")"
[[ "$count" -eq 3 ]] || fail "the scan found $count of 3 planted lines:"$'\n'"$found"

found="$(scan "$SANDBOX/good.sh")" || fail "the scan itself failed"
[[ -z "$found" ]] || fail "the scan refused a line that reads the real status:"$'\n'"$found"

# --- 2. The real suites. --------------------------------------------------
shopt -s nullglob
suites=("$REPO"/tests/cli/*.sh "$REPO"/tests/cli-oracle/*.sh)
[[ ${#suites[@]} -gt 0 ]] || fail "no suites under tests/cli or tests/cli-oracle"

found="$(scan "${suites[@]}")" || fail "the scan itself failed"
if [[ -n "$found" ]]; then
    echo "FAIL  a check reads \$? after a command substitution, so it tests that, not the tool:" >&2
    printf '%s\n' "${found//$REPO\//}" >&2
    exit 1
fi

echo "PASS  every CLI check reads the tool's own status (${#suites[@]} suites)"
