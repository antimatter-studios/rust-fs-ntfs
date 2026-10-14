#!/usr/bin/env bash
# sparse-layout-oracle.sh BINARY IMAGE SPEC... -- Windows' sparse layout as
# the oracle for this crate's.
#
# Each SPEC is `/name@bytes@regions`, as win-write-sparse.ps1 takes it: a
# file of `bytes` bytes with data only in `regions` (`OFFSET+LENGTH,...` or
# `none`). Windows has already written /name into IMAGE with that content and
# made it sparse. This writes the same content beside it as /ours-name with
# `rust-ntfs sparse --regions`, and then `rust-ntfs layout` must print the
# same text for both: the same flags, compression unit, sizes, the same
# ranges of data and holes, and the same value. Cluster numbers are not in
# that text, so where each file landed does not matter; how it is stored does.
#
# Every file is compared, and every layout printed, before the verdict, so
# one run shows all of what Windows did. Exit 0 when every pair agrees, 1
# when any differs or cannot be written or read, 2 for a usage error.
set -uo pipefail

if [ "$#" -lt 3 ]; then
    echo "usage: sparse-layout-oracle.sh BINARY IMAGE /name@bytes@regions..." >&2
    exit 2
fi
binary=$1
image=$2
shift 2

# Git Bash must not turn the volume path /name into C:/Program Files/Git/name
# before the native binary sees it.
export MSYS_NO_PATHCONV=1

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

fails=0
for spec in "$@"; do
    IFS=@ read -r path bytes regions <<<"$spec"
    name="${path#/}"
    if [ -z "$name" ] || [ -z "${bytes:-}" ] || [ -z "${regions:-}" ] || [[ $name == */* ]]; then
        echo "sparse-layout-oracle: '$spec' is not /name@bytes@regions in the volume root" >&2
        exit 2
    fi
    ours="ours-$name"

    echo "== $path ($bytes bytes, regions $regions)"
    if ! "$binary" layout "$image" "/$name" >"$work/windows.txt" 2>"$work/err.txt"; then
        echo "   FAIL  Windows' /$name cannot be read: $(cat "$work/err.txt")"
        fails=$((fails + 1))
        continue
    fi
    if ! { "$binary" touch "$image" / "$ours" &&
        "$binary" sparse "$image" "/$ours" --bytes "$bytes" --regions "$regions" &&
        "$binary" layout "$image" "/$ours" >"$work/ours.txt"; } >"$work/out.txt" 2>"$work/err.txt"; then
        echo "   FAIL  /$ours cannot be written or read: $(cat "$work/out.txt" "$work/err.txt")"
        fails=$((fails + 1))
        continue
    fi
    sed 's/^/   windows  /' "$work/windows.txt"
    if diff -u --label "windows /$name" --label "ours /$ours" \
        "$work/windows.txt" "$work/ours.txt" >"$work/diff.txt"; then
        echo "   ok    /$ours is laid out as Windows laid out /$name"
    else
        echo "   FAIL  /$ours is not laid out as Windows laid out /$name:"
        sed 's/^/         /' "$work/diff.txt"
        fails=$((fails + 1))
    fi
done

if [ "$fails" -gt 0 ]; then
    echo "sparse-layout-oracle: $fails of $# file(s) differ from Windows' layout"
    exit 1
fi
echo "sparse-layout-oracle: all $# file(s) laid out as Windows lays them out"
