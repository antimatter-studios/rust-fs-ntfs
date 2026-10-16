#!/usr/bin/env bash
# A failing $I30 capture must keep its manifest and raw-scan diagnostic, and
# its failure must stay a failure. The evidence writer is driven twice in a
# scratch directory: once with an injected empty raw scan (the producer
# failure) and once with one attested entry (the positive control).
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
fails=0
fail() { fails=$((fails + 1)); printf 'FAIL %s\n' "$*"; }
ok() { :; }
helper="$ROOT/test-disks/i30-slack-evidence.ps1"
capture="$ROOT/test-disks/capture-i30-slack.ps1"
workflow="$ROOT/.github/workflows/ci.yml"

[[ -f "$helper" ]] || fail 'evidence writer is missing: test-disks/i30-slack-evidence.ps1'
grep -Fq 'i30-slack-evidence.ps1' "$capture" || fail 'capture script does not use the evidence writer'
grep -Fq 'Write-I30SlackEvidence' "$capture" || fail 'capture script does not write evidence through Write-I30SlackEvidence'

# The always-run upload keeps only the small text files, never the image.
step="$(awk '/- name: Upload \$I30 capture diagnostics/{on=1} on&&/^      - name:/&&!/Upload \$I30 capture diagnostics/{exit} on' "$workflow")"
[[ -n "$step" ]] || fail 'CI has no "Upload $I30 capture diagnostics" step'
grep -Fq 'if: always()' <<<"$step" || fail 'diagnostic upload is not if: always()'
grep -Fq 'test-disks/ntfs-i30-slack.manifest.json' <<<"$step" || fail 'diagnostic upload omits the manifest'
grep -Fq 'test-disks/ntfs-i30-slack.diagnostic.txt' <<<"$step" || fail 'diagnostic upload omits the raw-scan diagnostic'
grep -Fq '.img' <<<"$step" && fail 'diagnostic upload retains a disk image'
grep -Fq 'retention-days:' <<<"$step" || fail 'diagnostic upload has no retention bound'

if ! command -v pwsh >/dev/null 2>&1; then
    fail 'pwsh is required to drive the evidence writer: install PowerShell on the runner'
else
    tmp="$(mktemp -d)"
    trap 'rm -rf "$tmp"' EXIT
    cat >"$tmp/drive.ps1" <<'PS'
param([string]$Helper, [string]$Out, [int]$Attested)
$ErrorActionPreference = 'Stop'
. $Helper
$evidence = @()
if ($Attested -gt 0) {
    $evidence = @([ordered]@{ name = 'reused-name.txt'; file_id = [uint64]281474976710700; record_number = 44; block_image_offset = 8192; vcn = 0; entry_offset_in_block = 88 })
}
$manifest = [ordered]@{
    schema = 'rust-fs-ntfs-i30-slack-capture-v1'
    directory_file_id = [uint64]281474976710693
    operations = @(
        [ordered]@{ operation = 'create'; name = 'reused-name.txt'; role = 'deleted'; file_id = [uint64]281474976710700 },
        [ordered]@{ operation = 'delete'; name = 'reused-name.txt' }
    )
    image_sha256 = 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa'
    raw_deleted_evidence = $evidence
    raw_scan = [ordered]@{ indx_blocks = 2; deleted_name_hits = $Attested }
}
Write-I30SlackEvidence -Manifest $manifest -ManifestPath (Join-Path $Out 'm.json') -DiagnosticPath (Join-Path $Out 'd.txt')
PS
    mkdir -p "$tmp/neg" "$tmp/pos"

    # Negative: the injected empty scan fails red, names the precondition, and
    # still leaves both files bound to the image hash and operation order.
    pwsh -NoProfile -File "$tmp/drive.ps1" -Helper "$helper" -Out "$tmp/neg" -Attested 0 >"$tmp/neg.out" 2>&1
    status=$?
    (( status != 0 )) || fail 'empty raw scan exited 0: the precondition failure was swallowed'
    grep -Fq 'PRECONDITION i30-raw-deleted-candidate' "$tmp/neg.out" || fail 'failure does not name PRECONDITION i30-raw-deleted-candidate'
    [[ -s "$tmp/neg/m.json" ]] || fail 'failed capture lost its manifest'
    [[ -s "$tmp/neg/d.txt" ]] || fail 'failed capture lost its raw-scan diagnostic'
    grep -Fq '"image_sha256": "aaaaaaaa' "$tmp/neg/m.json" 2>/dev/null || fail 'retained manifest is not bound to the image hash'
    grep -Fq 'image_sha256=aaaaaaaa' "$tmp/neg/d.txt" 2>/dev/null || fail 'retained diagnostic is not bound to the image hash'
    grep -Fq 'attested=0' "$tmp/neg/d.txt" 2>/dev/null || fail 'retained diagnostic does not record the empty scan'
    grep -Fq 'operation[0]=create name=reused-name.txt' "$tmp/neg/d.txt" 2>/dev/null || fail 'retained diagnostic lost the first operation'
    grep -Fq 'operation[1]=delete name=reused-name.txt' "$tmp/neg/d.txt" 2>/dev/null || fail 'retained diagnostic lost the operation order'

    # Positive: one attested entry passes and writes the same evidence.
    pwsh -NoProfile -File "$tmp/drive.ps1" -Helper "$helper" -Out "$tmp/pos" -Attested 1 >"$tmp/pos.out" 2>&1
    status=$?
    (( status == 0 )) || fail "attested capture exited $status: $(tail -n 3 "$tmp/pos.out")"
    grep -Fq 'attested=1' "$tmp/pos/d.txt" 2>/dev/null || fail 'positive diagnostic does not record the attested entry'
    grep -Fq 'evidence=reused-name.txt record=44 block_image_offset=8192 entry_offset_in_block=88' "$tmp/pos/d.txt" 2>/dev/null \
        || fail 'positive diagnostic does not name the attested entry'
fi

if (( fails == 0 )); then printf 'i30-slack-diagnostic-retention.sh: all checks passed\n'; else exit 1; fi
