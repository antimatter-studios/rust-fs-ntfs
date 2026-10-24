#!/usr/bin/env bash
# The split-extents scenario's counts outgrow the record its own image has
# (#1190).
#
# `cli-oracle.sh attrlist-split-extents` cuts a volume's free space into
# one-cluster holes and grows /split.dat's unnamed stream over them, a run
# per hole, until its run list needs more than one MFT record and is split
# into extents its attribute list names (#799). The stream is then cut to
# half, which must still need two (#800). The counts, 800 runs over 900
# holes, were chosen for 1024-byte records. The scenario's own image is
# made by cli-mkfs with mkfs.ntfs's default 4096-byte records, and the step
# refused it: "800 runs fit in one 4096-byte record" (Slate run
# 40013149279).
#
# scripts/split-extents-plan.sh RECORD CLUSTER VOLUME_BYTES computes the
# counts, and the recipe takes them from it. This holds the plan for the
# scenario's geometry (mkfs.ntfs's defaults, test-matrix.json's size) to:
#   - a run list longer than one record, at three bytes a one-cluster run;
#   - still longer than one record once the stream is cut to half;
#   - more holes than runs;
#   - two one-cluster files per hole, a cluster and a record each, and the
#     stream's spare, fitting the volume.
# It also holds test-matrix.json's notes to the counts the plan gives.
#
#   bash tests/scripts/split-extents-plan.sh
set -uo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
plan="$REPO/scripts/split-extents-plan.sh"
matrix="$REPO/test-matrix.json"
format="$REPO/src/bin/rust_ntfs/format.rs"
scenario=cli-attrlist-split-extents-win-verify-chkdsk
fails=0

fail() { echo "FAIL  $*" >&2; fails=$((fails + 1)); }

command -v jq >/dev/null 2>&1 || fail "jq is missing: apt-get install jq"
[ -x "$plan" ] || fail "scripts/split-extents-plan.sh is missing or not executable"

# The image cli-mkfs makes: `mkfs.ntfs --size`, so mkfs.ntfs's own record
# and cluster sizes.
const() { sed -n "s/^pub const $1: u32 = \\([0-9]*\\);\$/\\1/p" "$format"; }
record="$(const DEFAULT_MFT_RECORD_SIZE)"
cluster="$(const DEFAULT_CLUSTER_SIZE)"
[ -n "$record" ] && [ -n "$cluster" ] ||
    fail "no DEFAULT_MFT_RECORD_SIZE or DEFAULT_CLUSTER_SIZE in src/bin/rust_ntfs/format.rs"
size_mib="$(jq -r --arg s "$scenario" '.scenarios[$s].volume_params.size_mib // empty' "$matrix" 2>/dev/null)"
[ -n "$size_mib" ] || fail "test-matrix.json has no $scenario with a volume size"

if [ "$fails" -eq 0 ]; then
    bytes=$((size_mib << 20))
    if out="$("$plan" "$record" "$cluster" "$bytes" 2>&1)"; then
        read -r holes clusters <<<"$out"
        [ $((clusters * 3)) -gt "$record" ] ||
            fail "$clusters runs fit in one $record-byte record: the run list would not split"
        [ $((clusters / 2 * 3)) -gt "$record" ] ||
            fail "cut to half, $((clusters / 2)) runs fit in one $record-byte record: the cut stream would not stay split"
        [ "$holes" -gt "$clusters" ] || fail "$holes holes for $clusters runs: the stream cannot take a run per hole"
        need=$((2 * holes * (cluster + record) + clusters / 4 * cluster))
        [ "$need" -le "$bytes" ] ||
            fail "$((2 * holes)) one-cluster files and the spare take $need bytes, more than the $size_mib MiB volume"
        notes="$(jq -r --arg s "$scenario" '.scenarios[$s]._notes' "$matrix")"
        case "$notes" in
            *"$((2 * holes)) one-cluster files"*) ;;
            *) fail "$scenario's notes do not say $((2 * holes)) one-cluster files" ;;
        esac
        case "$notes" in
            *"to $clusters clusters"*) ;;
            *) fail "$scenario's notes do not say the stream grows to $clusters clusters" ;;
        esac
    else
        fail "split-extents-plan.sh $record $cluster $bytes refused the scenario's own volume: $out"
    fi
fi

# A 1024-byte record keeps the counts the recipe was first run with (#799).
if [ -x "$plan" ]; then
    out="$("$plan" 1024 4096 $((64 << 20)) 2>&1)"
    [ "$out" = "900 800" ] || fail "a 1024-byte record plans '$out', not '900 800'"
    if out="$("$plan" 4096 2048 $((128 << 20)) 2>&1)"; then
        fail "clusters smaller than a record are planned ('$out'): a one-cluster file stays resident"
    fi
    if out="$("$plan" 4096 4096 $((16 << 20)) 2>&1)"; then
        fail "a 16 MiB volume is planned ('$out'), though it cannot hold the one-cluster files"
    fi
fi

# The recipe takes its counts from the plan, not from numbers of its own.
recipe="$(awk '/^    attrlist-split-extents\)/ { on = 1 } on && /^        ;;$/ { exit } on' "$REPO/scripts/cli-oracle.sh")"
[ -n "$recipe" ] || fail "scripts/cli-oracle.sh has no attrlist-split-extents step"
case "$recipe" in
    *split-extents-plan.sh*) ;;
    *) fail "cli-oracle.sh attrlist-split-extents does not take its counts from split-extents-plan.sh" ;;
esac
if printf '%s\n' "$recipe" | grep -Eq '^[[:space:]]*(holes|clusters)=[0-9]+$'; then
    fail "cli-oracle.sh attrlist-split-extents still sets holes or clusters to a number of its own"
fi

[ "$fails" -eq 0 ] || exit 1
echo "split-extents-plan: all checks passed"
