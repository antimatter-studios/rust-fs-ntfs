#!/usr/bin/env bash
# classify-interrupted-captures.sh tells a replay that refused, and wrote
# nothing, from one that ran and left the volume with findings (#1231).
# fsck.ntfs exits 4 for both: the second wrote damage, and reporting it as a
# refusal hid that on all six `freed` snapshots of run 39051606379.
#
# FSCK is a stand-in that answers from what each pre-image holds, so the
# script's reading of fsck.ntfs's report is tested without a volume.
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
mkdir -p "$ROOT/tmp"
WORK=$(mktemp -d "$ROOT/tmp/classify-interrupted.XXXXXX")
trap 'rm -rf "$WORK"' EXIT
fails=0
ok() { printf 'ok %s\n' "$1"; }
fail() { printf 'FAIL %s\n' "$1"; fails=$((fails + 1)); }

cat > "$WORK/fsck.ntfs" <<'EOF'
#!/usr/bin/env bash
# fsck.ntfs [-y] --offset N IMAGE: the report each shape gives.
repair=0
[ "$1" = -y ] && repair=1
shape="$(cat "${!#}")"
if [ "$repair" = 0 ]; then
    echo '{"logfile":"records"}'
    exit 4
fi
log='"kind":"logfile","why":"$LogFile holds records after its last checkpoint"'
case "$shape" in
    replayed)
        echo "{\"exit\":1,\"findings\":[{$log,\"repaired\":true,\"replayed\":975}]}"
        exit 1 ;;
    replayed-dirty)
        echo "{\"exit\":4,\"findings\":[{$log,\"repaired\":true,\"replayed\":12},{\"kind\":\"dirty\",\"repairable\":false,\"why\":\"left for chkdsk\"}]}"
        exit 4 ;;
    damaged)
        echo "{\"exit\":4,\"findings\":[{$log,\"repaired\":true,\"replayed\":975},{\"kind\":\"bad_mft_record\",\"record\":41,\"why\":\"its attributes reach the 384 bytes in use with no end marker\",\"repairable\":false}]}"
        exit 4 ;;
    refused)
        echo '{"exit":4,"findings":[{"kind":"logfile","repaired":false,"replayed":null,"why":"an operation not yet replayed"}]}'
        exit 4 ;;
    clean)
        echo '{"exit":0,"findings":[]}'
        exit 0 ;;
esac
exit 8
EOF
chmod +x "$WORK/fsck.ntfs"

dir="$WORK/captures"
mkdir -p "$dir"
echo '{"partition_offset":0}' > "$dir/layout.json"
k=0
for shape in replayed replayed-dirty damaged refused clean; do
    k=$((k + 1))
    printf '%s' "$shape" > "$dir/pre-$k.vhd"
done

FSCK="$WORK/fsck.ntfs" bash "$ROOT/scripts/classify-interrupted-captures.sh" "$dir" > "$WORK/out" 2>&1
status=$?
row() { awk -F'\t' -v k="$1" '$1 == k' "$dir/classification.tsv"; }
outcome() { row "$1" | cut -f3; }

[ "$status" -eq 1 ] && ok 'a replay that left findings fails the run' \
    || fail "a replay that left findings exited $status"
[ "$(grep -c . "$dir/classification.tsv" 2>/dev/null)" -eq 6 ] && ok 'every snapshot is classified, damaged or not' \
    || fail 'classification.tsv does not hold a header and five snapshots'
[ "$(outcome 1)" = replayed ] && ok 'a clean replay is replayed' || fail "a clean replay is $(outcome 1)"
[ "$(outcome 2)" = replayed ] && ok 'a replay leaving only the dirty flag is replayed' \
    || fail "a replay leaving only the dirty flag is $(outcome 2)"
[ "$(outcome 3)" = damaged ] && ok 'a replay leaving findings is damaged' \
    || fail "a replay leaving findings is $(outcome 3)"
row 3 | grep -qF '975 redo operations' && ok 'a damaged row says how much was replayed' \
    || fail "a damaged row does not say how much was replayed: $(row 3)"
row 3 | grep -qF 'record 41: its attributes reach the 384 bytes in use with no end marker' \
    && ok 'a damaged row names what the replay left' \
    || fail "a damaged row does not name what the replay left: $(row 3)"
[ "$(outcome 4)" = refused ] && ok 'a refusal is refused' || fail "a refusal is $(outcome 4)"
row 4 | grep -qF 'an operation not yet replayed' && ok 'a refusal keeps its reason' \
    || fail "a refusal lost its reason: $(row 4)"
[ "$(outcome 5)" = nothing-to-replay ] && ok 'a clean log is nothing-to-replay' \
    || fail "a clean log is $(outcome 5)"
grep -qF 'damaged' "$WORK/out" && ok 'the damage is printed' || fail 'the damage is not printed'

if (( fails > 0 )); then exit 1; fi
echo 'classify-interrupted-captures: all checks passed'
