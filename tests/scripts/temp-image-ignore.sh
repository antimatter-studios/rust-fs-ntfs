#!/usr/bin/env bash
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
sandbox="$(mktemp -d)"
trap 'rm -r "$sandbox"' EXIT
fails=0

git init --quiet "$sandbox" || exit 1
cp "$ROOT/.gitignore" "$sandbox/.gitignore" || exit 1
mkdir -p "$sandbox/test-disks" || exit 1

image="test-disks/_windows-interrupted-1_61751_221.img"
for suffix in '' .copy-repaired .in-place .changed .repaired .refused \
    .resumed .cancelled .capi .capi.plan .plan .applied .link .other .to \
    .arbitrary-new-suffix; do
    path="$image$suffix"
    : > "$sandbox/$path" || exit 1
    if ! git -C "$sandbox" -c core.excludesFile=/dev/null \
        check-ignore --no-index --quiet -- "$path"; then
        echo "FAIL: generated scratch image is not ignored: $path" >&2
        fails=$((fails + 1))
    fi
done

for suffix in .capi.d .journal.d _journals; do
    directory="$image$suffix"
    mkdir -p "$sandbox/$directory/nested" || exit 1
    path="$directory/nested/repair.journal"
    : > "$sandbox/$path" || exit 1
    if ! git -C "$sandbox" -c core.excludesFile=/dev/null \
        check-ignore --no-index --quiet -- "$path"; then
        echo "FAIL: derived scratch directory is not ignored: $path" >&2
        fails=$((fails + 1))
    fi
done

status="$(git -C "$sandbox" -c core.excludesFile=/dev/null \
    status --porcelain --untracked-files=all -- test-disks)" || exit 1
if [ -n "$status" ]; then
    echo "FAIL: interrupted scratch artifacts leave git status dirty" >&2
    fails=$((fails + 1))
fi

for path in test-disks/windows-interrupted-1.img.gz \
    test-disks/windows-interrupted-1.img.notes.md \
    test-disks/_scratch.rs docs/_example.img.changed test-disks/.gitkeep; do
    git -C "$sandbox" -c core.excludesFile=/dev/null \
        check-ignore --no-index --quiet -- "$path"
    if [ "$?" -ne 1 ]; then
        echo "FAIL: non-scratch path must remain visible: $path" >&2
        fails=$((fails + 1))
    fi
done

[ "$fails" -eq 0 ] || exit 1
echo "temp-image-ignore.sh: all checks passed"
