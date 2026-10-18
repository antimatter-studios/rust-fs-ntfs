#!/usr/bin/env bash
# The fixed set of generated operation samples the ntfs-3g oracle run keeps
# (#1034): one per boundary geometry plus the named-stream and EA sequences.
# Each sample directory must hold the formatted image the sequence left,
# the geometry it was formatted at and ntfs-3g's report on that image, so a
# Windows job can grade the same bytes with chkdsk (#1035).
set -euo pipefail
samples=(
    boundary-geometry-c512-r1024-v1
    boundary-geometry-c512-r4096-v1
    boundary-geometry-c65536-r1024-v1
    boundary-geometry-c65536-r4096-v1
    streams
    eas
)
# `--list` prints the set comma-separated for OPSEQ_RETAIN_SAMPLES, so the
# oracle run keeps exactly the samples this check requires.
if [ "${1:-}" = "--list" ]; then
    (IFS=,; echo "${samples[*]}")
    exit 0
fi
root="${1:-tmp/op-sequence-model}"
fails=0
for sample in "${samples[@]}"; do
    for file in sample.img geometry.txt ntfsfix.txt; do
        if [ ! -s "${root}/${sample}/${file}" ]; then
            echo "retained sample ${sample}: ${file} missing" >&2
            fails=$((fails + 1))
        fi
    done
done
# The four boundary samples cover every cluster/record pair they name.
for pair in "512 1024" "512 4096" "65536 1024" "65536 4096"; do
    read -r cluster record <<<"$pair"
    if ! cat "${root}"/boundary-*/geometry.txt 2>/dev/null |
        awk -v c="$cluster" -v r="$record" '$2 == c && $3 == r { found = 1 } END { exit !found }'; then
        echo "no retained sample at cluster ${cluster}, record ${record}" >&2
        fails=$((fails + 1))
    fi
done
if [ "$fails" -gt 0 ]; then
    echo "op-sequence samples: ${fails} problems across ${#samples[@]} samples" >&2
    exit 1
fi
echo "op-sequence samples: ${#samples[@]} retained"
