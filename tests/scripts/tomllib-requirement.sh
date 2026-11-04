#!/usr/bin/env bash
# Everything that needs tomllib, run by a python3 older than 3.11, fails with
# one line naming the version and what provides it, not with a traceback or a
# list of unrelated failed checks (#1265). None of them skips: the status stays
# a failure, and with no python3 at all a guard fails the same way.
#
# The stand-in reports 3.9.6 and cannot import tomllib whichever python3 is
# real, so this runs the same on a runner with a newer one.
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
work="$(mktemp -d "${TMPDIR:-/tmp}/tomllib-requirement.XXXXXX")"
trap 'rm -rf "$work"' EXIT
fails=0
bad() { echo "FAIL $*"; fails=$((fails + 1)); }

guards="run-host-steps.sh slate-writer-capture.sh win-enumerate-optional-paths.sh windows-index-allocation-insertion.sh"
tools="run-host-steps check-matrix"

real="$(command -v python3)" || {
    echo 'FAIL python3 is required to drive the stand-in: install Python and put it on PATH'
    exit 1
}
mkdir "$work/old" "$work/no-tomllib" "$work/none"
printf 'raise ModuleNotFoundError("No module named %s", name="tomllib")\n' "'tomllib'" \
    >"$work/no-tomllib/tomllib.py"
cat >"$work/old/python3" <<EOF
#!/bin/sh
if [ "\${1:-}" = --version ]; then echo 'Python 3.9.6'; exit 0; fi
PYTHONDONTWRITEBYTECODE=1 PYTHONPATH="$work/no-tomllib" exec "$real" "\$@"
EOF
chmod +x "$work/old/python3"
for tool in bash basename dirname; do
    ln -s "$(command -v "$tool")" "$work/none/$tool"
done

# The stand-in is what it claims to be, or everything below proves nothing.
PATH="$work/old:$PATH" python3 -c 'import tomllib' 2>/dev/null &&
    bad 'the stand-in python3 imports tomllib'

# one NAME EXPECTED-TEXT COMMAND...: a failing status, one line, the text.
one() {
    local name="$1" text="$2" out rc lines
    shift 2
    out="$("$@" 2>&1)"
    rc=$?
    lines="$(printf '%s\n' "$out" | wc -l)"
    [ "$rc" -ne 0 ] || bad "$name: exits 0"
    [ "$lines" -eq 1 ] || bad "$name: prints $((lines)) lines, not one"
    grep -Fq 'Traceback' <<<"$out" && bad "$name: prints a traceback"
    grep -Fq "$text" <<<"$out" || bad "$name: does not say '$text'"
    grep -Fq 'tomllib' <<<"$out" || bad "$name: does not name tomllib"
}

for guard in $guards; do
    one "$guard under 3.9" 'Python 3.11 or newer' \
        env PATH="$work/old:$PATH" bash "$ROOT/tests/scripts/$guard"
    one "$guard without python3" 'no python3 on PATH' \
        env PATH="$work/none" "$work/none/bash" "$ROOT/tests/scripts/$guard"
done
for tool in $tools; do
    one "$tool under 3.9" 'Python 3.11 or newer' \
        env PATH="$work/old:$PATH" python3 "$ROOT/scripts/fs-windows-test-harness/$tool"
done

# The prerequisites are written where a developer looks first.
for doc in README.md AGENTS.md; do
    for needed in 'Python 3.11' PyYAML PowerShell 'chore test:scripts'; do
        grep -Fq "$needed" "$ROOT/$doc" || bad "$doc does not name $needed"
    done
done

if [ "$fails" -gt 0 ]; then
    exit 1
fi
echo 'tomllib-requirement.sh: all checks passed'
