#!/usr/bin/env bash
# mac-write-streams.sh BINARY IMAGE PATH COUNT SIZE DELETE_EVERY
#
# Give the file at PATH the named streams s1..sCOUNT through `rust-ntfs
# stream`, stream i holding "stream <i> " repeated to SIZE bytes: the
# contents win-write-streams.ps1 writes and win-verify-streams.ps1 checks.
# A DELETE_EVERY of k > 0 then deletes every k-th stream (s<k>, s<2k>, ...);
# 0 deletes none. Enough streams outgrow the file's base record, so the
# file has to be given an $ATTRIBUTE_LIST and extension records (#480).
set -euo pipefail

binary=$1
image=$2
path=$3
count=$4
size=$5
every=$6

for n in "$count" "$size" "$every"; do
    [[ $n =~ ^[0-9]+$ ]] || { echo "count, size and delete-every must be nonnegative integers" >&2; exit 2; }
done

for ((i = 1; i <= count; i++)); do
    unit="stream $i "
    content=""
    while ((${#content} < size)); do content+=$unit; done
    # Git Bash must not turn the volume path /f into C:/Program Files/Git/f.
    MSYS_NO_PATHCONV=1 "$binary" stream "$image" "$path" "s$i" --content "${content:0:size}" >/dev/null
done
if ((every > 0)); then
    for ((i = every; i <= count; i += every)); do
        MSYS_NO_PATHCONV=1 "$binary" stream "$image" "$path" "s$i" --delete >/dev/null
    done
fi
