#!/usr/bin/env bash
# The status summary must agree with the explicit states in the body. The
# source report contains 56 High and 44 Medium findings; Low findings are not
# tracked by this ledger.
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
STATUS="$ROOT/docs/human-code-status.md"

body_counts=$(awk '
    /^### [A-G][0-9]+ — / {
        id = $2
        if (seen[id]++) {
            printf "  FAIL  duplicate finding ID %s\n", id > "/dev/stderr"
            bad = 1
        }
        if ($0 ~ / — \*\*fixed( earlier)?\*\*$/) fixed++
        else if ($0 ~ / — \*\*documented and pinned\*\*$/) documented++
        else if ($0 ~ / — \*\*partially fixed;/) {
            partial++
            if (id != "B4" || $0 !~ /\[#140\]/) bad = 1
        }
    }
    END { print fixed + 0, documented + 0, partial + 0, bad + 0 }
' "$STATUS")
read -r fixed_body documented_body partial_body bad_body <<< "$body_counts"
fixed_summary=$(awk -F'|' '$2 ~ /^ Fixed / { gsub(/ /, "", $3); print $3 }' "$STATUS")
documented_summary=$(awk -F'|' '$2 ~ /^ Documented and pinned/ { print $3 + 0 }' "$STATUS")
open_summary=$(awk -F'|' '$2 ~ /^ Still open / { print $3 + 0 }' "$STATUS")
open_heading=$(sed -n 's/^## Still open — \([0-9][0-9]*\) High and Medium$/\1/p' "$STATUS")
expected_open=$((56 + 44 - fixed_body - documented_body))

pass=0; fail=0
check_equal() {
    local name="$1" got="$2" want="$3"
    if [ "$got" = "$want" ]; then
        pass=$((pass + 1))
    else
        fail=$((fail + 1))
        printf '  FAIL  %s (got %s, wanted %s)\n' "$name" "$got" "$want"
    fi
}

printf 'human-code-status\n'
check_equal "fixed summary agrees with body" "$fixed_summary" "$fixed_body"
check_equal "documented summary agrees with body" "$documented_summary" "$documented_body"
check_equal "open summary is the remaining High/Medium findings" "$open_summary" "$expected_open"
check_equal "open heading agrees with summary" "$open_heading" "$open_summary"
check_equal "one partial finding remains open" "$partial_body" 1
check_equal "body IDs and partial status are unambiguous" "$bad_body" 0

if grep -Eq '^### B4 .*— \*\*partially fixed; same-length path tracked by \[#140\]' "$STATUS"; then
    pass=$((pass + 1))
else
    fail=$((fail + 1))
    printf '  FAIL  B4 must remain partial and link existing issue #140\n'
fi

printf '  %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
