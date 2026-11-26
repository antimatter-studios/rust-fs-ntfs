#!/usr/bin/env bash
# Every file a test tier or fixture producer writes under test-disks/ is
# ignored, so a worktree that ran the suite stays clean (#1367, #1403).
# The list is the outputs found untracked in real worktrees, plus every
# test-disks/ path a workflow uploads and git does not track: a new upload
# with no ignore rule fails here.
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
sandbox="$(mktemp -d)"
trap 'rm -rf "$sandbox"' EXIT
fails=0

git init --quiet "$sandbox/repo" || exit 1
cp "$ROOT/.gitignore" "$sandbox/repo/.gitignore" || exit 1
mkdir -p "$sandbox/repo/test-disks" || exit 1
cp "$ROOT/test-disks/.gitignore" "$sandbox/repo/test-disks/.gitignore" || exit 1

known="
test-disks/logfile-checkpoint-provenance.json
test-disks/ntfs-4kn-read.lifecycle.log
test-disks/ntfs-attrlist.streams.tsv
test-disks/ntfs-attrlist.streams.tsv.bytes/0123456789abcdef.bin
test-disks/ntfs-cli-read.directory.json
test-disks/ntfs-ea.query.tsv
test-disks/ntfs-i30-slack.diagnostic.txt
test-disks/ntfs-i30-slack.manifest.json
test-disks/ntfs-inventory.fileids.txt
test-disks/ntfs-stat.streams.tsv
test-disks/ntfs-stat.streams.tsv.bytes/0123456789abcdef.bin
test-disks/usn-journal-data.wrong-id-control.tsv
test-disks/usn-journal-identity-controls.txt
test-disks/usn-read-probe.tsv
test-disks/usn-read-probe.wrong-id-control.tsv
test-disks/usn-v2-capture.diagnostics.txt
"

# Upload entries are whole lines holding one path. A directory entry is
# probed through a file inside it; a glob entry is probed as a sample name.
uploaded="$(sed -n -E 's#^[[:space:]]+(path:[[:space:]]+)?(test-disks/[^[:space:]]+)[[:space:]]*$#\2#p' \
    "$ROOT"/.github/workflows/*.yml | sed -e 's#/$#/sample.bin#' -e 's#\*#sample#g' | sort -u)"
if [ -z "$uploaded" ]; then
    echo "FAIL: no uploaded test-disks/ path was found in the workflows" >&2
    exit 1
fi

checked=0
for path in $known $uploaded; do
    git -C "$ROOT" ls-files --error-unmatch -- "$path" >/dev/null 2>&1 && continue
    checked=$((checked + 1))
    if ! git -C "$sandbox/repo" -c core.excludesFile=/dev/null \
        check-ignore --no-index --quiet -- "$path"; then
        echo "FAIL: generated output is not ignored: $path" >&2
        fails=$((fails + 1))
    fi
done

# Controls: sources and documents beside the outputs stay visible.
for path in test-disks/new-capture.ps1 test-disks/new-fixture.sh \
    test-disks/new-captures.json test-disks/new.manifest test-disks/new.img.gz; do
    git -C "$sandbox/repo" -c core.excludesFile=/dev/null \
        check-ignore --no-index --quiet -- "$path"
    if [ "$?" -ne 1 ]; then
        echo "FAIL: a source path must remain visible: $path" >&2
        fails=$((fails + 1))
    fi
done

[ "$fails" -eq 0 ] || exit 1
echo "generated-outputs-ignored.sh: all checks passed ($checked outputs)"
