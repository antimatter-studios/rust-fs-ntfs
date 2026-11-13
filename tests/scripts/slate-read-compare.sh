#!/usr/bin/env bash
# scripts/slate-read-compare.py compares the paths, kinds and streams Slate
# reads from an image with the manifest Windows recorded from the same image
# (#1103, part of #857). Windows is the reference; a comparison that cannot
# fail is no comparison, so each way the two can disagree is planted here and
# must fail by case, path and stream.
#
# Exact agreement passes. A missing or extra path, a wrong kind, a missing or
# extra stream, a wrong size and wrong bytes each fail by name. A manifest
# bound to another image hash, or naming another case, is refused before any
# entry is compared. An entry Slate cannot read is only excused by a line in
# the capability list, and every excused entry is reported.
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/slate-read-compare.XXXXXX")"
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

printf 'case\torigin\timage\tsha256\twindows_verdict\twindows_exit\nwin\twindows\twin.img\t%s\tclean\t0\n' "$IMG" >"$WORK/cases.tsv"

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

# run SLATE_ENTRIES [SLATE_CASE] [SLATE_HASH] [WINDOWS_HASH]: compare Slate's
# read of case `win` with Windows' reference manifest.
run() {
    rm -rf "$WORK/windows" "$WORK/slate" "$WORK/out"
    manifest "$WORK/windows" win "${4:-$IMG}" "$REFERENCE"
    manifest "$WORK/slate" win "${3:-$IMG}" "$1"
    if [ -n "${2:-}" ]; then
        sed "s/\"case\":\"win\"/\"case\":\"$2\"/" "$WORK/slate/win.json" >"$WORK/slate/tmp" &&
            mv "$WORK/slate/tmp" "$WORK/slate/win.json"
    fi
    python3 "$ROOT/scripts/slate-read-compare.py" "$WORK/cases.tsv" "$WORK/windows" "$WORK/slate" \
        "${CAPS:-/dev/null}" "$WORK/out" >"$WORK/log" 2>&1
}

# fails_with WHAT ENTRIES ROW: Slate's ENTRIES fail, and diff.tsv names ROW.
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
grep -q $'^win\t3\t3\tagree$' "$WORK/out/cases.tsv" 2>/dev/null && ok ||
    bad "the per-case table counts both sides' entries: $(cat "$WORK/out/cases.tsv" 2>/dev/null)"

# Slate's order does not matter: Windows' ordinal order is not Slate's job.
run "$(printf '%s' "$REFERENCE" | python3 -c 'import json,sys; print(json.dumps(list(reversed(json.load(sys.stdin)))))')"
[ "$?" -eq 0 ] && ok || bad "the same entries in another order: $(cat "$WORK/log")"

drop() { printf '%s' "$REFERENCE" | python3 -c "import json,sys; e=json.load(sys.stdin); $1; print(json.dumps(e))"; }

fails_with "a path Slate omits" "$(drop 'e.pop(1)')" $'win\t/dir/empty\t\tmissing-path'
fails_with "a path Slate adds" \
    "$(drop 'e.append({"path":"/extra","kind":"file","streams":[{"name":"","size":0,"sha256":"'$EMPTY'"}]})')" \
    $'win\t/extra\t\textra-path'
fails_with "a directory Slate reads as a file" "$(drop 'e[0]["kind"]="file"')" $'win\t/dir\t\tkind'
fails_with "a named stream Slate omits" "$(drop 'e[2]["streams"].pop(1)')" $'win\t/dir/ünï\tads\tmissing-stream'
fails_with "a stream Slate adds" \
    "$(drop 'e[1]["streams"].append({"name":"zone","size":1,"sha256":"'$ONE'"})')" \
    $'win\t/dir/empty\tzone\textra-stream'
fails_with "a size Slate reads differently" "$(drop 'e[2]["streams"][0]["size"]=6')" $'win\t/dir/ünï\t\tsize'
fails_with "bytes Slate reads differently" "$(drop 'e[0]["streams"][0]["sha256"]="'$ONE'"')" $'win\t/dir\tnote\tsha256'

run "$REFERENCE" "" "$OTHER"
rc=$?
[ "$rc" -eq 1 ] && ok || bad "Slate's manifest of another image: exit $rc, want 1"
grep -q "image" "$WORK/log" && grep -q "$OTHER" "$WORK/log" && ok ||
    bad "the refusal names the wrong image hash: $(cat "$WORK/log")"

run "$REFERENCE" "" "" "$OTHER"
[ "$?" -eq 1 ] && ok || bad "Windows' manifest of another image: exit 0, want 1"

run "$REFERENCE" other
rc=$?
[ "$rc" -eq 1 ] && ok || bad "a manifest naming another case: exit $rc, want 1"
grep -q "other" "$WORK/log" && ok || bad "the refusal names the other case: $(cat "$WORK/log")"

run "$REFERENCE"
rm -f "$WORK/slate/win.json"
python3 "$ROOT/scripts/slate-read-compare.py" "$WORK/cases.tsv" "$WORK/windows" "$WORK/slate" \
    /dev/null "$WORK/out" >"$WORK/log" 2>&1
rc=$?
[ "$rc" -eq 1 ] && ok || bad "a case Slate produced no manifest for: exit $rc, want 1"

# The capability list excuses an entry Slate cannot read, by name, and says so.
printf 'win\t/dir/ünï\tads\tnamed streams on this file are not read by the tool\n' >"$WORK/caps.tsv"
CAPS="$WORK/caps.tsv" run "$(drop 'e[2]["streams"].pop(1)')"
rc=$?
[ "$rc" -eq 0 ] && ok || bad "an omission the capability list excuses: exit $rc, want 0: $(cat "$WORK/log")"
grep -qF $'win\t/dir/ünï\tads\tnamed streams' "$WORK/out/excluded.tsv" 2>/dev/null && ok ||
    bad "the excused entry is reported in excluded.tsv: $(cat "$WORK/out/excluded.tsv" 2>/dev/null)"
CAPS="$WORK/caps.tsv" run "$(drop 'e[2]["streams"][0]["size"]=6')"
[ "$?" -eq 1 ] && ok || bad "an exclusion for one stream does not excuse another"

python3 "$ROOT/scripts/slate-read-compare.py" >"$WORK/log" 2>&1
[ "$?" -eq 2 ] && ok || bad "no arguments is a usage error"

FLOOR=27
if [ $((pass + fail)) -lt "$FLOOR" ]; then
    bad "ran $((pass + fail)) checks, below the floor of $FLOOR"
fi
if [ "$fail" -gt 0 ]; then
    echo "slate-read-compare.sh: $fail of $((pass + fail)) checks failed"
    exit 1
fi
echo "slate-read-compare.sh: all checks passed"
