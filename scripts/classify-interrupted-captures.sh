#!/usr/bin/env bash
# classify-interrupted-captures.sh DIR -- say, for every pre-image the
# logfile-oracle workflow captured into DIR, what this crate's $LogFile
# replay makes of it, so a person can pick the fixture each refused shape
# of #137 needs without reading log bytes by hand.
#
# For each DIR/pre-K.vhd (the partition at layout.json's partition_offset):
#   1. fsck.ntfs, read-only: what the log holds (empty, clean, records);
#   2. fsck.ntfs -y on a copy: replayed (with the redo count), or the
#      reason the replay refused it -- the reason names the shape: a
#      transaction to undo, a log that wraps, a page spanning clusters,
#      an operation not yet replayed.
# Writes DIR/classification.tsv: K, log state, outcome, reason. Changes no
# pre-image. The verdict on a replay is not this script's: a candidate
# that replays here is still graded against Windows' own recovery
# (post-K.vhd, post-K.manifest.tsv) before it becomes a fixture.
#
# FSCK names the fsck.ntfs to run (default: fsck.ntfs on PATH).
set -euo pipefail

dir="${1:?usage: classify-interrupted-captures.sh DIR}"
fsck="${FSCK:-fsck.ntfs}"
command -v "$fsck" >/dev/null 2>&1 || {
    echo "classify: no $fsck: build it with \`cargo build --release --locked --features cli --bin rust-fs-ntfs\` and link fsck.ntfs to it" >&2
    exit 1
}
command -v jq >/dev/null 2>&1 || {
    echo "classify: jq is required (apt-get install jq)" >&2
    exit 1
}
offset="$(jq -er .partition_offset "$dir/layout.json")"
out="$dir/classification.tsv"
printf 'snapshot\tlog\toutcome\treason\n' >"$out"

shopt -s nullglob
pres=("$dir"/pre-*.vhd)
[ "${#pres[@]}" -gt 0 ] || {
    echo "classify: no pre-*.vhd in $dir" >&2
    exit 1
}
for pre in "${pres[@]}"; do
    k="$(basename "$pre" .vhd)"
    k="${k#pre-}"
    set +e
    state="$("$fsck" --offset "$offset" "$pre" 2>/dev/null | jq -r '.logfile // "unreadable"')"
    work="$dir/.classify-$k.vhd"
    cp "$pre" "$work"
    report="$("$fsck" -y --offset "$offset" "$work" 2>&1)"
    status=$?
    set -e
    rm -f "$work"
    if [ "$status" -eq 1 ]; then
        outcome="replayed"
        reason="$(jq -r '[.findings[] | select(.kind == "logfile") | .replayed] | first // "" | "\(.) redo operations"' <<<"$report" 2>/dev/null || echo "")"
    elif [ "$status" -eq 0 ]; then
        outcome="nothing-to-replay"
        reason=""
    else
        outcome="refused"
        reason="$(jq -r '[.findings[] | select(.kind == "logfile") | .why] | first // .error // ""' <<<"$report" 2>/dev/null || printf '%s' "$report" | head -c 300)"
    fi
    printf '%s\t%s\t%s\t%s\n' "$k" "$state" "$outcome" "$reason" >>"$out"
done
sort -n -o "$out" "$out"
cat "$out"
