#!/usr/bin/env bash
# The Windows-authored $I30 slack capture must retain its provenance in CI.
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
fail=0
bad() { fail=$((fail + 1)); printf 'FAIL %s\n' "$*"; }
ok() { :; }
script="$ROOT/test-disks/capture-i30-slack.ps1"
workflow="$ROOT/.github/workflows/ci.yml"
[[ -f "$script" ]] || bad 'Windows $I30 create/delete/reuse capture script is missing: test-disks/capture-i30-slack.ps1'
if [[ -f "$script" ]]; then
    grep -Fq 'ntfs-i30-slack.img' "$script" || bad 'Windows-authored $I30 capture image path is missing from the generator'
    grep -Fq 'ntfs-i30-slack.manifest.json' "$script" || bad 'capture provenance manifest path is missing from the generator'
    grep -Fq 'authored_by' "$script" || bad 'capture manifest omits Windows author provenance'
    grep -Fq 'image_sha256' "$script" || bad 'capture manifest omits image SHA-256 provenance'
    grep -Fq 'reused-name.txt' "$script" || bad 'capture does not create/delete/reuse the same index name'
fi
grep -Fq 'test-disks/capture-i30-slack.ps1' "$workflow" || bad 'CI does not run the Windows $I30 capture script'
grep -Fq 'test-disks/ntfs-i30-slack.img' "$workflow" || bad 'CI does not upload the $I30 image'
grep -Fq 'test-disks/ntfs-i30-slack.manifest.json' "$workflow" || bad 'CI does not upload the provenance manifest'
if (( fail == 0 )); then printf 'i30-slack-artifact.sh: all checks passed\n'; else exit 1; fi
