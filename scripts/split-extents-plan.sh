#!/usr/bin/env bash
# split-extents-plan.sh RECORD CLUSTER VOLUME_BYTES -- the counts
# `cli-oracle.sh attrlist-split-extents` works with on a volume of
# VOLUME_BYTES, RECORD-byte MFT records and CLUSTER-byte clusters. Prints
# `HOLES CLUSTERS`: how many one-cluster holes the free space is cut into,
# and how many clusters /split.dat's unnamed stream grows to over them, a
# run per hole.
#
# THE COUNTS FOLLOW THE RECORD (#1190). They were chosen for 1024-byte
# records: 800 runs over 900 holes (#799). A one-cluster run two clusters
# from the last takes three bytes of mapping pairs, so 800 runs are 2400
# bytes, more than one record holds, and cut to half (#800) their 400 runs
# still need two extents. On the 4096-byte records mkfs.ntfs writes by
# default, 800 runs fit in one record, and the step refused its own image.
# So the runs are 800 x RECORD / 1024, never fewer than 800, and the holes
# 9/8 of them, as at 1024.
#
# REFUSED, exit 1 naming why:
#   - CLUSTER smaller than RECORD: a one-cluster file would stay resident
#     and leave no hole behind;
#   - a volume that cannot hold 2 x HOLES one-cluster files, a cluster and
#     a record each, the quarter of the stream's clusters left spare beside
#     the filler, and an eighth of itself for its own metadata: the log,
#     the system files and the index of the directory the files are in.
set -euo pipefail

usage() { echo "usage: split-extents-plan.sh RECORD CLUSTER VOLUME_BYTES" >&2; exit 2; }
die() { echo "split-extents-plan: $*" >&2; exit 1; }

[ $# -eq 3 ] || usage
record="$1"
cluster="$2"
bytes="$3"
for v in "$record" "$cluster" "$bytes"; do
    case "$v" in ''|*[!0-9]*) echo "split-extents-plan: '$v' is not a whole number" >&2; usage ;; esac
done

[ "$cluster" -ge "$record" ] ||
    die "$cluster-byte clusters under $record-byte records: a one-cluster file would stay resident and leave no hole"
clusters=$((800 * record / 1024))
[ "$clusters" -ge 800 ] || clusters=800
holes=$((clusters * 9 / 8))
need=$((2 * holes * (cluster + record) + clusters / 4 * cluster + bytes / 8))
[ "$need" -le "$bytes" ] ||
    die "$((2 * holes)) one-cluster files, the spare and the volume's own metadata take $need bytes, more than its $bytes"
echo "$holes $clusters"
