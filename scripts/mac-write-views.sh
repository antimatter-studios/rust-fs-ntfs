#!/usr/bin/env bash
# Make `count` symlinks `l0000`.. under `parent`, each to `target.txt`
# beside them, and give each an object id, so that this crate puts that many
# entries into $Extend\$Reparse:$R and $Extend\$ObjId:$O (#571).
#
# A symlink's $R key is its tag and file reference, so each new one sorts
# after those before it. The object ids spread over the key space: the
# first word is the link's number times an odd constant, which no two share,
# so they land in every leaf of $O.
set -euo pipefail

binary=$1
image=$2
parent=$3
count=$4

[[ $count =~ ^[0-9]+$ ]] || { echo "count must be a nonnegative integer" >&2; exit 2; }
for ((i = 0; i < count; i++)); do
    printf -v name 'l%04d' "$i"
    printf -v id '%08x%08x%08x%08x' $(( ((i + 1) * 0x9E3779B1) & 0xFFFFFFFF )) "$i" 0x05710571 0
    # Git Bash must not turn the volume path /views into C:/Program Files/Git/views.
    MSYS_NO_PATHCONV=1 "$binary" symlink "$image" "$parent" "$name" target.txt >/dev/null
    MSYS_NO_PATHCONV=1 "$binary" objid "$image" "${parent%/}/$name" "$id" >/dev/null
done
