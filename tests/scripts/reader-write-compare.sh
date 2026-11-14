#!/usr/bin/env bash
# scripts/reader-write-compare.py compares our reader's read of every
# Slate-written copy Windows accepted with the manifest Windows recorded from
# the same copy (#1115, part of #865). Windows is the reference and nothing
# is excused: every way the two can disagree is planted here and must fail by
# case, path and stream.
#
# Exact agreement passes. A missing or extra path, a wrong kind, a missing or
# extra stream, a wrong size and wrong bytes each fail by name. A manifest
# bound to another image hash, or naming another case, is refused. Only an
# `accepted` row of slate-writes-graded.tsv is compared: a copy Windows
# rejected is a Slate defect and never grades our reader. A table with no
# accepted row compares nothing, and that is a failure, not a pass.
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/reader-write-compare.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT
pass=0
fail=0
ok() { pass=$((pass + 1)); }
bad() { fail=$((fail + 1)); printf 'FAIL %s\n' "$1"; }

IMG=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
OTHER=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb
EMPTY=e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855
ONE=1111111111111111111111111111111111111111111111111111111111111111
TWO=2222222222222222222222222222222222222222222222222222222222222222
HEADER=$'case\timage\tsha256\twindows_verdict\twindows_exit'

# The Windows grading table (#864): one accepted copy, one Slate defect and
# one copy the writer never left. Only `wrote` may be compared.
printf '%s\nwrote\twrote.img\t%s\taccepted\t0\nbroken\tbroken.img\t%s\tslate-defect\t3\nlost\t-\t-\tnot-written\t-\n' \
    "$HEADER" "$IMG" "$OTHER" >"$WORK/graded.tsv"

# manifest DIR CASE HASH ENTRIES: one schema-1 manifest, ENTRIES a JSON list.
manifest() {
    mkdir -p "$1"
    printf '{"schema":1,"case":"%s","image_sha256":"%s","oracle":"test","capabilities":{},"entries":%s}\n' \
        "$2" "$3" "$4" >"$1/$2.json"
}

REFERENCE='[
 {"path":"/dir","kind":"directory","streams":[{"name":"note","size":3,"sha256":"'$TWO'"}]},
 {"path":"/dir/empty","kind":"file","streams":[{"name":"","size":0,"sha256":"'$EMPTY'"}]},
 {"path":"/dir/ünï","kind":"file","streams":[{"name":"","size":5,"sha256":"'$ONE'"},{"name":"ads","size":3,"sha256":"'$TWO'"}]}
]'

compare() {
    python3 "$ROOT/scripts/reader-write-compare.py" "${GRADED:-$WORK/graded.tsv}" "$WORK/windows" \
        "$WORK/reader" "$WORK/out" >"$WORK/log" 2>&1
}

# run READER_ENTRIES [READER_CASE] [READER_HASH] [WINDOWS_HASH]: compare our
# reader's read of case `wrote` with Windows' reference manifest.
run() {
    rm -rf "$WORK/windows" "$WORK/reader" "$WORK/out"
    manifest "$WORK/windows" wrote "${4:-$IMG}" "$REFERENCE"
    manifest "$WORK/reader" wrote "${3:-$IMG}" "$1"
    if [ -n "${2:-}" ]; then
        sed "s/\"case\":\"wrote\"/\"case\":\"$2\"/" "$WORK/reader/wrote.json" >"$WORK/reader/tmp" &&
            mv "$WORK/reader/tmp" "$WORK/reader/wrote.json"
    fi
    compare
}

# fails_with WHAT ENTRIES ROW: our reader's ENTRIES fail, and diff.tsv names ROW.
fails_with() {
    run "$2"
    local rc=$?
    [ "$rc" -eq 1 ] && ok || bad "$1: exit $rc, want 1: $(cat "$WORK/log")"
    grep -qF -- "$3" "$WORK/out/diff.tsv" 2>/dev/null && ok ||
        bad "$1: diff.tsv lacks '$3': $(cat "$WORK/out/diff.tsv" 2>/dev/null)"
}

run "$REFERENCE"
rc=$?
[ "$rc" -eq 0 ] && ok || bad "exact agreement: exit $rc, want 0: $(cat "$WORK/log")"
[ "$(grep -c . "$WORK/out/diff.tsv" 2>/dev/null)" = 1 ] && ok ||
    bad "exact agreement leaves only the header in diff.tsv: $(cat "$WORK/out/diff.tsv" 2>/dev/null)"
grep -q $'^wrote\t3\t3\tagree$' "$WORK/out/cases.tsv" 2>/dev/null && ok ||
    bad "the per-case table counts both sides' entries: $(cat "$WORK/out/cases.tsv" 2>/dev/null)"

# A Slate defect and a copy never written are listed, never compared: neither
# has a manifest here, and neither may be refused for lacking one.
grep -q $'^broken\t\t\tnot-compared slate-defect$' "$WORK/out/cases.tsv" 2>/dev/null && ok ||
    bad "a copy Windows rejected is listed as not compared: $(cat "$WORK/out/cases.tsv" 2>/dev/null)"
grep -q $'^lost\t\t\tnot-compared not-written$' "$WORK/out/cases.tsv" 2>/dev/null && ok ||
    bad "a copy the writer never left is listed as not compared: $(cat "$WORK/out/cases.tsv" 2>/dev/null)"
grep -q 'broken' "$WORK/out/diff.tsv" 2>/dev/null && bad "a Slate defect graded our reader" || ok

# Our reader's order does not matter: Windows' ordinal order is not ours.
run "$(printf '%s' "$REFERENCE" | python3 -c 'import json,sys; print(json.dumps(list(reversed(json.load(sys.stdin)))))')"
[ "$?" -eq 0 ] && ok || bad "the same entries in another order: $(cat "$WORK/log")"

drop() { printf '%s' "$REFERENCE" | python3 -c "import json,sys; e=json.load(sys.stdin); $1; print(json.dumps(e))"; }

fails_with "a path our reader omits" "$(drop 'e.pop(1)')" $'wrote\t/dir/empty\t\tmissing-path'
fails_with "a path our reader adds" \
    "$(drop 'e.append({"path":"/extra","kind":"file","streams":[{"name":"","size":0,"sha256":"'$EMPTY'"}]})')" \
    $'wrote\t/extra\t\textra-path'
fails_with "a directory our reader reads as a file" "$(drop 'e[0]["kind"]="file"')" $'wrote\t/dir\t\tkind'
fails_with "a named stream our reader omits" "$(drop 'e[2]["streams"].pop(1)')" $'wrote\t/dir/ünï\tads\tmissing-stream'
fails_with "a stream our reader adds" \
    "$(drop 'e[1]["streams"].append({"name":"zone","size":1,"sha256":"'$ONE'"})')" \
    $'wrote\t/dir/empty\tzone\textra-stream'
fails_with "a size our reader reads differently" "$(drop 'e[2]["streams"][0]["size"]=6')" $'wrote\t/dir/ünï\t\tsize'
fails_with "bytes our reader reads differently" "$(drop 'e[0]["streams"][0]["sha256"]="'$ONE'"')" $'wrote\t/dir\tnote\tsha256'

run "$REFERENCE" "" "$OTHER"
rc=$?
[ "$rc" -eq 1 ] && ok || bad "our reader's manifest of another image: exit $rc, want 1"
grep -q "$OTHER" "$WORK/log" && ok || bad "the refusal names the wrong image hash: $(cat "$WORK/log")"
grep -q $'^wrote\t\t\trefused$' "$WORK/out/cases.tsv" 2>/dev/null && ok ||
    bad "a refused case is recorded as refused: $(cat "$WORK/out/cases.tsv" 2>/dev/null)"

run "$REFERENCE" "" "" "$OTHER"
[ "$?" -eq 1 ] && ok || bad "Windows' manifest of another image: exit 0, want 1"

run "$REFERENCE" other
rc=$?
[ "$rc" -eq 1 ] && ok || bad "a manifest naming another case: exit $rc, want 1"
grep -q "other" "$WORK/log" && ok || bad "the refusal names the other case: $(cat "$WORK/log")"

run "$REFERENCE"
rm -f "$WORK/reader/wrote.json"
compare
[ "$?" -eq 1 ] && ok || bad "an accepted copy our reader produced no manifest for: exit 0, want 1"

# Nothing accepted means nothing compared: no evidence is not agreement.
printf '%s\nbroken\tbroken.img\t%s\tslate-defect\t3\n' "$HEADER" "$OTHER" >"$WORK/none.tsv"
GRADED="$WORK/none.tsv" run "$REFERENCE"
rc=$?
[ "$rc" -eq 1 ] && ok || bad "a table with no accepted copy: exit $rc, want 1: $(cat "$WORK/log")"

# A table that is not the grading table is refused, not half-read.
printf 'case\torigin\timage\tsha256\twindows_verdict\twindows_exit\nwrote\tslate\twrote.img\t%s\tclean\t0\n' \
    "$IMG" >"$WORK/cases.tsv"
GRADED="$WORK/cases.tsv" run "$REFERENCE"
rc=$?
[ "$rc" -eq 2 ] && ok || bad "a table with another header: exit $rc, want 2: $(cat "$WORK/log")"

# There is no capability list: a fifth argument is a usage error.
python3 "$ROOT/scripts/reader-write-compare.py" "$WORK/graded.tsv" "$WORK/windows" "$WORK/reader" \
    /dev/null "$WORK/out" >"$WORK/log" 2>&1
[ "$?" -eq 2 ] && ok || bad "an exclusion list is not accepted"
python3 "$ROOT/scripts/reader-write-compare.py" >"$WORK/log" 2>&1
[ "$?" -eq 2 ] && ok || bad "no arguments is a usage error"

FLOOR=32
if [ $((pass + fail)) -lt "$FLOOR" ]; then
    bad "ran $((pass + fail)) checks, below the floor of $FLOOR"
fi
if [ "$fail" -gt 0 ]; then
    echo "reader-write-compare.sh: $fail of $((pass + fail)) checks failed"
    exit 1
fi
echo "reader-write-compare.sh: all checks passed"
