#!/usr/bin/env bash
# needs-windows-gate.sh BASE_SHA -- print `true` when the change from BASE_SHA
# to HEAD needs the Windows chkdsk gate, `false` when it does not.
#
# THE CODE THAT DECIDES BYTES ON DISK, plus the workflow that validates them.
# Deliberately not "any .rs": a change to the read path cannot make chkdsk
# unhappy about a volume, and paying for Windows on it teaches people to merge
# without reading the gate. ci.yml's `changes` job runs this on pull requests.
#
# THE TOOLS THE WINDOWS JOB RUNS COUNT TOO (#403). `HARNESS_REF` decides every
# command the matrix sends to Windows and `IMG_VHD_REF` builds the image chkdsk
# reads; a pull request that moves either has to meet the gate it changes. So
# does scripts/run-matrix.sh, which every matrix step calls. Only those two
# pin lines of chores.yml count, not the whole file.
#
# So does tests/scripts/chkdsk-verdict.ps1, the test of the chkdsk verdict:
# it runs in the Windows job, and nowhere else is a change to it run (#419).
set -euo pipefail

base="${1:?usage: needs-windows-gate.sh BASE_SHA}"
changed="$(git diff --name-only "$base"...HEAD)"

if echo "$changed" | grep -qE '^(src/(mkfs|sds|record_build|idx_block|index_io|write|fsck)\.rs|src/bin/rust_ntfs/format\.rs|src/cli/|scripts/cli-oracle\.sh|scripts/run-matrix\.sh|scripts/needs-windows-gate\.sh|test-matrix\.json|fs-windows-test-harness\.toml|scripts/fs-windows-test-harness/|tests/scripts/chkdsk-verdict\.ps1|\.github/workflows/ci\.yml)'; then
    echo true
    exit 0
fi
if git diff "$base"...HEAD -- chores.yml | grep -qE '^[-+][[:space:]]*(HARNESS_REF|IMG_VHD_REF):'; then
    echo true
    exit 0
fi
echo false
