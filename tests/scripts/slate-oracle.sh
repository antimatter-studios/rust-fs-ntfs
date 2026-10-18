#!/usr/bin/env bash
# scripts/slate-oracle.sh runs Slate's offline checker over images Windows
# chkdsk has already graded, and compares the two verdicts (#687). A
# comparison that cannot fail is no comparison, so this drives it with a
# stand-in checker whose answer is the first line of each fake image:
# `clean` exits 0, `findings` exits 4 (Slate's EXIT_FINDINGS), `crash` exits 8
# (EXIT_FAILURE), and `write` makes the image writable and changes it.
#
# Agreement passes and writes the table. Each way a run can be incomplete
# or wrong fails: a disagreement, a missing image, a hash that is not the one
# Windows graded, a checker that changes its input, a checker exit that is
# neither clean nor a finding, a missing checker, a case Windows did not
# accept, and a run below its floor. `fetch` refuses a checkout whose commit,
# licence digest or declared crate licence is not the pinned one.
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/slate-oracle.XXXXXX")"
trap 'chmod -R u+w "$WORK" 2>/dev/null; rm -rf "$WORK"' EXIT
pass=0
fail=0
ok() { pass=$((pass + 1)); }
bad() { fail=$((fail + 1)); printf 'FAIL %s\n' "$1"; }
sha() { if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1"; else shasum -a 256 "$1"; fi | cut -d' ' -f1; }

cat >"$WORK/checker" <<'SH'
#!/usr/bin/env bash
[ "$1" = --check ] || { echo "stand-in: expected --check, got $*" >&2; exit 16; }
case "$(head -n 1 "$2")" in
    clean) echo "no problems found"; exit 0 ;;
    findings) echo "index entry out of order"; exit 4 ;;
    crash) echo "cannot read boot sector" >&2; exit 8 ;;
    write) chmod u+w "$2" && echo changed >>"$2"; exit 0 ;;
    *) exit 16 ;;
esac
SH
chmod +x "$WORK/checker"

# image NAME CONTENT: a fake image in $WORK/images.
mkdir -p "$WORK/images"
image() { printf '%s\n' "$2" >"$WORK/images/$1"; }
image win-4k.img clean
image ours-4k.img clean
image disagree.img findings
image crash.img crash
image write.img write

# cases ROW...: a cases table, one `case origin image windows_verdict` per
# argument; the hash is the image's own unless the row gives a fifth field.
cases() {
    printf 'case\torigin\timage\tsha256\twindows_verdict\twindows_exit\n' >"$WORK/cases.tsv"
    for row in "$@"; do
        # shellcheck disable=SC2086 # a row is split into its fields on purpose
        set -- $row
        local digest="${5:-}"
        [ -n "$digest" ] || digest="$(sha "$WORK/images/$3" 2>/dev/null || echo absent)"
        printf '%s\t%s\t%s\t%s\t%s\t0\n' "$1" "$2" "$3" "$digest" "$4" >>"$WORK/cases.tsv"
    done
}

compare() {
    rm -rf "$WORK/out"
    bash "$ROOT/scripts/slate-oracle.sh" compare "$WORK/cases.tsv" "$WORK/images" \
        "${CHECKER:-$WORK/checker}" "$WORK/out" "$1" >"$WORK/log" 2>&1
}

cases 'win-4k windows win-4k.img clean' 'ours-4k ours ours-4k.img clean'
compare 2
rc=$?
[ "$rc" -eq 0 ] && ok || bad "two agreeing cases: exit $rc, want 0: $(cat "$WORK/log")"
[ "$(grep -c $'\tagree$' "$WORK/out/verdicts.tsv" 2>/dev/null)" = 2 ] && ok ||
    bad "the table records both agreements: $(cat "$WORK/out/verdicts.tsv" 2>/dev/null)"
grep -q $'^ours-4k\tours\t[0-9a-f]\\{64\\}\tclean\t0\tclean\t0\tagree$' "$WORK/out/verdicts.tsv" 2>/dev/null && ok ||
    bad "a row carries case, origin, hash and both verdicts with exit codes"
[ -s "$WORK/out/ours-4k.slate.txt" ] && ok || bad "the checker's own report is kept for each case"

cases 'win-4k windows win-4k.img clean' 'disagree ours disagree.img clean'
compare 2
rc=$?
[ "$rc" -ne 0 ] && ok || bad "Windows clean and Slate finding problems: exit 0, want a failure"
grep -q 'disagree' "$WORK/log" && ok || bad "a disagreement names its case: $(cat "$WORK/log")"
grep -q $'^disagree\t.*\tdisagree$' "$WORK/out/verdicts.tsv" 2>/dev/null && ok ||
    bad "a disagreement is in the table"

cases 'win-4k windows win-4k.img clean' 'gone ours gone.img clean'
compare 1
rc=$?
[ "$rc" -ne 0 ] && ok || bad "a missing image: exit 0, want a failure"
grep -q 'gone' "$WORK/log" && ok || bad "a missing image names its case: $(cat "$WORK/log")"

cases "win-4k windows win-4k.img clean $(printf '0%.0s' $(seq 64))"
compare 1
rc=$?
[ "$rc" -ne 0 ] && ok || bad "an image that is not the one Windows graded: exit 0, want a failure"

cases 'write ours write.img clean'
before="$(sha "$WORK/images/write.img")"
compare 1
rc=$?
[ "$rc" -ne 0 ] && ok || bad "a checker that changes the image: exit 0, want a failure"
grep -qi 'changed' "$WORK/log" && ok || bad "a changed image is reported as such: $(cat "$WORK/log")"
[ "$(sha "$WORK/images/write.img")" = "$before" ] && ok ||
    bad "the checker is given a copy, so the image Windows graded is left as it was"

cases 'crash ours crash.img clean'
compare 1
rc=$?
[ "$rc" -ne 0 ] && ok || bad "a checker exit that is neither clean nor a finding: exit 0, want a failure"
grep -q 'exit 8' "$WORK/log" && ok || bad "an unexpected checker exit is quoted: $(cat "$WORK/log")"

cases 'win-4k windows win-4k.img clean'
CHECKER="$WORK/no-such-checker" compare 1
rc=$?
[ "$rc" -ne 0 ] && ok || bad "a missing checker: exit 0, want a failure"

cases 'win-4k windows win-4k.img not-clean'
compare 1
rc=$?
[ "$rc" -ne 0 ] && ok || bad "a case Windows did not accept: exit 0, want a failure"
grep -q $'\twindows-rejected$' "$WORK/out/verdicts.tsv" 2>/dev/null && ok ||
    bad "a case Windows did not accept is never counted as an agreement"

cases 'win-4k windows win-4k.img clean' 'ours-4k ours ours-4k.img clean'
compare 3
rc=$?
[ "$rc" -ne 0 ] && ok || bad "two cases against a floor of three: exit 0, want a failure"
grep -q 'floor' "$WORK/log" && ok || bad "a run below its floor says so: $(cat "$WORK/log")"

# write: a stand-in for Slate's copy-only writer (#863), taking
# `SOURCE NEW ROOT_FILENAME OFFSET EXPECTED_HEX REPLACEMENT_HEX` as
# ntfs-write-lab does. The source's first line picks its behaviour: `copy`
# writes a changed copy, `scribble` also changes its source, `crash` exits 1,
# and `same` leaves the copy identical to its source.
cat >"$WORK/writer" <<'SH'
#!/usr/bin/env bash
[ "$#" -eq 6 ] || { echo "stand-in: expected six arguments, got $#" >&2; exit 16; }
[ "$3" = target.bin ] && [ "$4" = 8 ] && [ "$5" = 00ff ] && [ "$6" = abcd ] ||
    { echo "stand-in: unexpected arguments $*" >&2; exit 16; }
case "$(head -n 1 "$1")" in
    copy) cp "$1" "$2" && echo written >>"$2" ;;
    scribble) cp "$1" "$2" && echo written >>"$2"; chmod u+w "$1" && echo changed >>"$1" ;;
    crash) echo "expected bytes do not match" >&2; exit 1 ;;
    same) cp "$1" "$2" ;;
    *) exit 16 ;;
esac
SH
chmod +x "$WORK/writer"
image copy.img copy
image scribble.img scribble
image wcrash.img crash
image same.img same

# writes ROW...: a write table, one `case source` per argument, every row
# replacing 00ff with abcd at offset 8 of the root file target.bin.
writes() {
    printf 'case\tsource\troot_file\toffset\texpected_hex\treplacement_hex\n' >"$WORK/writes.tsv"
    for row in "$@"; do
        # shellcheck disable=SC2086 # a row is split into its fields on purpose
        set -- $row
        printf '%s\t%s\ttarget.bin\t8\t00ff\tabcd\n' "$1" "$2" >>"$WORK/writes.tsv"
    done
}
write() {
    rm -rf "$WORK/wout"
    bash "$ROOT/scripts/slate-oracle.sh" write "$WORK/writes.tsv" "$WORK/images" \
        "$WORK/writer" "$WORK/wout" "$1" >"$WORK/log" 2>&1
}

before="$(sha "$WORK/images/copy.img")"
writes 'copy copy.img'
write 1
rc=$?
[ "$rc" -eq 0 ] && ok || bad "a writer that changes only its copy: exit $rc, want 0: $(cat "$WORK/log")"
[ "$(sha "$WORK/images/copy.img")" = "$before" ] && ok || bad "the source image is unchanged after a write"
[ -f "$WORK/wout/images/copy.img" ] && [ "$(sha "$WORK/wout/images/copy.img")" != "$before" ] && ok ||
    bad "the written copy is kept in OUT/images and differs from its source"
grep -q $'^copy\tcopy.img\t'"$before"$'\t[0-9a-f]\\{64\\}\t0\twritten$' "$WORK/wout/slate-writes.tsv" 2>/dev/null && ok ||
    bad "slate-writes.tsv records the case, source, both hashes, exit and result: $(cat "$WORK/wout/slate-writes.tsv" 2>/dev/null)"

writes 'copy copy.img' 'scribble scribble.img'
write 1
[ "$?" -ne 0 ] && ok || bad "a writer that changes its source: exit 0, want a failure"
grep -q 'FAIL scribble:' "$WORK/log" && ok || bad "a changed source is named by case: $(cat "$WORK/log")"
[ "$(head -n 1 "$WORK/images/scribble.img")" = scribble ] && [ "$(wc -l <"$WORK/images/scribble.img")" -eq 1 ] && ok ||
    bad "the writer is never handed the source image itself"

writes 'wcrash wcrash.img'
write 0
[ "$?" -ne 0 ] && ok || bad "a writer that exits 1: exit 0, want a failure"
grep -q 'FAIL wcrash:' "$WORK/log" && ok || bad "a failed write is named by case: $(cat "$WORK/log")"

writes 'same same.img'
write 0
[ "$?" -ne 0 ] && ok || bad "a writer that leaves its copy unchanged: exit 0, want a failure"
grep -q 'FAIL same:' "$WORK/log" && ok || bad "an unchanged copy is named by case: $(cat "$WORK/log")"

writes 'gone absent.img'
write 0
[ "$?" -ne 0 ] && ok || bad "a missing source image: exit 0, want a failure"

writes 'copy copy.img'
write 2
[ "$?" -ne 0 ] && ok || bad "one written case against a floor of two: exit 0, want a failure"
grep -q 'floor' "$WORK/log" && ok || bad "a write run below its floor says so: $(cat "$WORK/log")"

# fetch: a local repository stands in for Slate's.
src="$WORK/slate-src"
mkdir -p "$src/src/tools"
printf 'MIT License\n' >"$src/LICENSE"
printf '[package]\nname = "slate-ntfs"\nlicense = "MIT"\n' >"$src/Cargo.toml"
printf '[package]\nname = "slate-ntfs-tools"\nlicense = "MIT"\n' >"$src/src/tools/Cargo.toml"
printf '# lock\n' >"$src/src/tools/Cargo.lock"
git -C "$src" init -q
git -C "$src" add -A
git -C "$src" -c user.name=t -c user.email=t@example.invalid commit -qm pinned
pinned="$(git -C "$src" rev-parse HEAD)"
printf 'GPL\n' >>"$src/LICENSE"
git -C "$src" -c user.name=t -c user.email=t@example.invalid commit -qam later
licence="$(git -C "$src" show "$pinned:LICENSE" | { if command -v sha256sum >/dev/null 2>&1; then sha256sum; else shasum -a 256; fi; } | cut -d' ' -f1)"

pins() { printf 'vars:\n  SLATE_URL: %s\n  SLATE_REF: %s\n  SLATE_LICENSE_SHA256: %s\n' "$src" "$1" "$2" >"$WORK/pins.yml"; }
fetch() {
    rm -rf "$WORK/slate"
    SLATE_PINS="$WORK/pins.yml" bash "$ROOT/scripts/slate-oracle.sh" fetch "$WORK/slate" >"$WORK/log" 2>&1
}

pins "$pinned" "$licence"
fetch
rc=$?
[ "$rc" -eq 0 ] && ok || bad "the pinned checkout: exit $rc, want 0: $(cat "$WORK/log")"
[ "$(git -C "$WORK/slate" rev-parse HEAD 2>/dev/null)" = "$pinned" ] && ok ||
    bad "fetch checks out the pinned commit, not the branch head"

pins "$(git -C "$src" rev-parse HEAD)" "$licence"
fetch
[ "$?" -ne 0 ] && ok || bad "a commit whose licence is not the pinned digest: exit 0, want a failure"

pins "${pinned:0:12}" "$licence"
fetch
[ "$?" -ne 0 ] && ok || bad "an abbreviated commit as the pin: exit 0, want a failure"

pins "$(printf 'f%.0s' $(seq 40))" "$licence"
fetch
[ "$?" -ne 0 ] && ok || bad "a pinned commit the repository does not hold: exit 0, want a failure"

git -C "$src" checkout -q -b relicensed "$pinned"
printf '[package]\nname = "slate-ntfs-tools"\nlicense = "MIT AND GPL-2.0-or-later"\n' >"$src/src/tools/Cargo.toml"
git -C "$src" -c user.name=t -c user.email=t@example.invalid commit -qam relicensed
pins "$(git -C "$src" rev-parse HEAD)" "$licence"
fetch
[ "$?" -ne 0 ] && ok || bad "a tools crate that does not declare MIT alone: exit 0, want a failure"

if [ "$fail" -gt 0 ]; then
    echo "slate-oracle.sh: $fail of $((pass + fail)) checks failed"
    exit 1
fi
echo "slate-oracle.sh: all checks passed"
