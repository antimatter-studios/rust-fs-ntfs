#!/usr/bin/env bash
# scripts/sparse-layout-oracle.sh compares how this crate lays out a sparse
# file with how Windows laid out the same content (#512). A comparison that
# cannot fail is no comparison, so this drives it with a stand-in binary
# whose `layout` answers are made up here: equal layouts pass, one differing
# file fails the run and is named while the other files are still compared,
# and a malformed entry is a usage error. The stand-in also records what it
# was asked, which must be the volume path untouched by Git Bash and the
# content the entry describes.
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/sparse-layout-oracle.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT
pass=0
fail=0
ok() { pass=$((pass + 1)); }
bad() { fail=$((fail + 1)); printf 'FAIL %s\n' "$1"; }

# `layout` prints compression_unit=4 for every file, except /ours-$DIFFER.
cat >"$WORK/fake" <<'SH'
#!/usr/bin/env bash
printf '%s|%s\n' "${MSYS_NO_PATHCONV:-}" "$*" >>"$CALLS"
case "$1" in
    layout)
        if [ "$3" = "/ours-${DIFFER:-}" ]; then echo "compression_unit=0"; else echo "compression_unit=4"; fi
        ;;
esac
SH
chmod +x "$WORK/fake"
export CALLS="$WORK/calls"

oracle() {
    : >"$CALLS"
    bash "$ROOT/scripts/sparse-layout-oracle.sh" "$WORK/fake" vol.img "$@" >"$WORK/out" 2>&1
}

export DIFFER=''
oracle /a.bin@400000@0+100,200000+100 /b.bin@65536@none
rc=$?
[ "$rc" -eq 0 ] && ok || bad "equal layouts: exit $rc, want 0: $(cat "$WORK/out")"
grep -qx '1|sparse vol.img /ours-a.bin --bytes 400000 --regions 0+100,200000+100' "$CALLS" &&
    ok || bad "our copy of /a.bin is written from its entry, paths unconverted: $(cat "$CALLS")"
grep -qx '1|touch vol.img / ours-b.bin' "$CALLS" && ok || bad "our copy of /b.bin is created in the root"

export DIFFER=a.bin
oracle /a.bin@400000@0+100 /b.bin@65536@none
rc=$?
[ "$rc" -eq 1 ] && ok || bad "one differing layout: exit $rc, want 1"
grep -q 'FAIL  /ours-a.bin is not laid out as Windows laid out /a.bin' "$WORK/out" &&
    ok || bad "the differing file is named: $(cat "$WORK/out")"
grep -q '+compression_unit=0' "$WORK/out" && ok || bad "the difference is shown"
grep -q 'ok    /ours-b.bin' "$WORK/out" && ok || bad "a failure does not stop the other files being compared"

oracle /a.bin@400000
rc=$?
[ "$rc" -eq 2 ] && ok || bad "an entry with no regions: exit $rc, want 2"
oracle /dir/a.bin@10@none
rc=$?
[ "$rc" -eq 2 ] && ok || bad "an entry outside the root: exit $rc, want 2"

if [ "$fail" -gt 0 ]; then
    echo "sparse-layout-oracle: $fail of $((pass + fail)) failed"
    exit 1
fi
echo "sparse-layout-oracle: $pass cases"
