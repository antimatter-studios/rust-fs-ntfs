#!/usr/bin/env bash
# matrix-keep-diag.sh keep          -- carry the last group's diagnostics aside
# matrix-keep-diag.sh seal STATUS   -- make what is about to be uploaded fit
#
# The harness runner removes test-diagnostics/matrix/ when a run starts, and
# the Windows ops write their evidence to the VM, so a job of many run-matrix.sh
# steps keeps only what is carried away before the next one starts (#1245).
#
# `keep` is what run-matrix.sh calls first when MATRIX_KEEP_DIAG names a
# directory: it fetches the VM's diagnostics for the group that last ran, with
# matrix-fetch-diag.sh, and moves test-diagnostics/matrix/ to
# $MATRIX_KEEP_DIAG/NN/. Groups are numbered, not merged by scenario, because a
# scenario can run in two groups: cli-fragmented-data-win-verify-chkdsk matches
# its own step's filter and the `cli` filter. The group that ran last stays at
# test-diagnostics/matrix/, where the job's fetch step and a reader of a failed
# run have always found it.
#
# `seal` runs before the upload. It removes a disk image, or anything the size
# of one, and fails naming it; it fails when the artifact is over its budget;
# and with STATUS `success` it fails unless every scenario kept its step output
# and, where a step ran on the VM, its VM report. A green run is then proof of
# what it kept. A failed run stops early, so a partial artifact is what it has.
#
# Neither says anything about a scenario: run-matrix.sh ignores what `keep`
# returns, and the job reads `seal` as a verdict of its own.
set -uo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MATRIX="$REPO/test-diagnostics/matrix"

# THE ARTIFACT'S BUDGET, in bytes before compression, is measured from these:
#   the cli group, 34 scenarios with vm/: 2,321,496 (run 41534865433)
#   the 32 other runs, at 32,618 each: win-format-win-quota-mac-streams-win-chkdsk,
#     step output and vm/, the largest other scenario with both kept (runs
#     41508136065 and 41356110931); 1,043,776 in all
#   diag/: 485,434 (run 41558177459)
# That is 3,850,706, which compressed at the cli group's 745,147 / 2,321,496 is
# about 1.2 MB. The budget is that plus about half, not the usual third: the 32
# other runs are extrapolated from one scenario until a green run prints the
# real total, and every run prints it below. Raise it with a measurement.
MAX_BYTES=6000000
# The largest file measured is 291,022 bytes (a cli-verify.txt) and the smallest
# image in test-matrix.json is 32 MiB, so this cannot catch a report and does
# catch an image under any name.
MAX_FILE_BYTES=4194304
# Distinct scenarios the job's filters select from test-matrix.json, measured
# at 64d52604: 27 filters, 66 runs, 65 scenarios. A group that was lost leaves no
# failure to read, only fewer scenarios. Raise it with the matrix.
MIN_SCENARIOS=65

keep() {
    local kept="$REPO/${MATRIX_KEEP_DIAG:?MATRIX_KEEP_DIAG names where to keep diagnostics}"
    # The job's first group has nothing before it.
    [ -d "$MATRIX" ] || return 0
    mkdir -p "$kept" || return 1

    local n=1 dir dest fetched
    for dir in "$kept"/*/; do
        [ -d "$dir" ] && n=$((n + 1))
    done
    dest="$(printf '%s/%02d' "$kept" "$n")"

    # The fetch's lines go into the group, and its count into this step's log:
    # this runs inside a budgeted tier, and the group that has something to say
    # is not the one this step is running.
    fetched="$(bash "$REPO/scripts/matrix-fetch-diag.sh" 2>&1)"
    printf '%s\n' "$fetched" > "$MATRIX/fetch.txt"
    mv "$MATRIX" "$dest" || return 1
    echo "[run-matrix] kept the last group as ${dest#"$REPO"/}: ${fetched##*$'\n'}"
}

seal() {
    local status="${1:?usage: matrix-keep-diag.sh seal JOB_STATUS}"
    local kept="$REPO/${MATRIX_KEEP_DIAG:?MATRIX_KEEP_DIAG names where to keep diagnostics}"
    local trees=() tree bad=0 f
    for tree in "$REPO/diag" "$MATRIX" "$kept"; do
        [ -d "$tree" ] && trees+=("$tree")
    done
    if [ "${#trees[@]}" -eq 0 ]; then
        echo "diagnostics: nothing to upload"
        [ "$status" != success ] && return 0
        echo "::error::a green job kept no diagnostics"
        return 1
    fi

    # No disk image reaches an artifact, whatever it is called.
    while IFS= read -r f; do
        rm -f -- "$f"
        echo "::error::removed ${f#"$REPO"/} before the upload: a disk image has no place in an artifact"
        bad=1
    done < <(find "${trees[@]}" -type f \( \
        -iname '*.img*' -o -iname '*.vhd*' -o -iname '*.vmdk' -o -iname '*.qcow2' \
        -o -iname '*.raw' -o -iname '*.iso' -o -size +"${MAX_FILE_BYTES}c" \))

    local bytes files
    bytes="$(find "${trees[@]}" -type f -exec cat {} + | wc -c | tr -d ' ')"
    files="$(find "${trees[@]}" -type f | wc -l | tr -d ' ')"
    echo "diagnostics: $bytes bytes in $files file(s), budget $MAX_BYTES"
    if [ "$bytes" -gt "$MAX_BYTES" ]; then
        echo "::error::the diagnostics are $bytes bytes, over the budget of $MAX_BYTES in scripts/matrix-keep-diag.sh"
        bad=1
    fi

    if [ "$status" = success ]; then
        local recipe dir name names=""
        while IFS= read -r recipe; do
            dir="$(dirname "$recipe")"
            name="$(basename "$dir")"
            names="$names$name"$'\n'
            if ! ls "$dir"/step-*/stdout.txt >/dev/null 2>&1; then
                echo "::error::$name kept no step output (${dir#"$REPO"/})"
                bad=1
            fi
            # Every step that ran on the VM is recorded with "host": "vm".
            if grep -q '"host": *"vm"' "$recipe" \
                && [ -z "$(find "$dir/vm" -type f 2>/dev/null | head -n 1)" ]; then
                echo "::error::$name ran on the VM and its report did not come home (${dir#"$REPO"/})"
                bad=1
            fi
        done < <(find "${trees[@]}" -type f -name recipe.json)
        local count
        count="$(printf '%s' "$names" | sort -u | grep -c .)"
        echo "diagnostics: $count scenario(s) kept, floor $MIN_SCENARIOS"
        if [ "$count" -lt "$MIN_SCENARIOS" ]; then
            echo "::error::a green job kept $count scenario(s), below the floor of $MIN_SCENARIOS in scripts/matrix-keep-diag.sh"
            bad=1
        fi
    fi
    [ "$bad" -eq 0 ]
}

case "${1:-}" in
    keep) keep ;;
    seal)
        shift
        seal "$@"
        ;;
    *)
        echo "usage: matrix-keep-diag.sh keep | seal JOB_STATUS" >&2
        exit 2
        ;;
esac
