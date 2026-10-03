# tests/scripts/chkdsk-verdict.ps1 -- the chkdsk verdict, driven with
# chkdsk's own reports and no VM.
#
#   pwsh -NoProfile -File tests/scripts/chkdsk-verdict.ps1
#
# Runs in the Windows chkdsk job before the matrix, and anywhere else
# PowerShell 7 is installed. Every fake chkdsk pass below returns an exit
# code and report that a real run produced; the run that produced each one
# is named beside it.

$ErrorActionPreference = 'Stop'
. "$PSScriptRoot/../../scripts/fs-windows-test-harness/_chkdsk-verdict.ps1"

$script:fails = 0
function Ok($what) { Write-Host "ok   $what" }
function Fail($what) { Write-Host "FAIL $what"; $script:fails++ }
function Check($cond, $what) { if ($cond) { Ok $what } else { Fail $what } }

# chkdsk <letter>: /scan on a hosted windows-latest runner, when the volume
# snapshot it scans could not be taken. Verbatim from the post-repair /scan of
# cli-corrupt-mirror-fsck-win-chkdsk, CI run 37074606783 (exit 10), and of
# cli-corrupt-signature-fsck-win-chkdsk, CI run 36734098054 (exit 10).
$SnapshotProviderError = @'
The type of the file system is NTFS.
The shadow copy provider had an error. Check the System and Application event logs for more information.

A snapshot error occured while scanning this drive. You can try again, but if this problem persists, run an offline scan and fix.
'@

# The same failure for a volume too small to hold a shadow copy, exit 11
# (rust-fs-ntfs#295, mac-format-tiny-32mib, 2026-09-18).
$SnapshotNoStorage = @'
The type of the file system is NTFS.
Insufficient storage available to create either the shadow copy storage file or other shadow copy data.
A snapshot error occured while scanning this drive. Run an offline scan and fix.
'@

# A pass that warned it could not make a shadow copy, scanned the live volume
# and found errors. Verbatim (first and last lines) from the read-only pass of
# cli-windows-interrupted-index-vcn-fsck-replay-win-verify-chkdsk, CI run
# 37136974900, exit 3 (rust-fs-ntfs#429).
$ShadowWarnedFound = @'
Insufficient storage available to create either the shadow copy storage file or other shadow copy data.
Stage 1: Examining basic file system structure ...
Stage 2: Examining file name linkage ...
Error detected in index $I30 for file 26.
Errors found.  CHKDSK cannot continue in read-only mode.
'@

$Clean = @'
The type of the file system is NTFS.
Windows has scanned the file system and found no problems.
No further action is required.
'@

$Found = @'
The type of the file system is NTFS.
Windows has checked the file system and found problems.
'@

$Fixed = @'
The type of the file system is NTFS.
Correcting errors in the Master File Table (MFT) mirror.
Windows has made corrections to the file system.
'@

# A fake chkdsk: answers each pass from $Table, keyed by "<mode><suffix>",
# and records the order it was called in.
function New-FakeChkdsk([hashtable]$Table) {
    $calls = [System.Collections.Generic.List[string]]::new()
    $invoke = {
        param($mode, $suffix)
        $key = "$mode$suffix"
        $calls.Add($key)
        if (-not $Table.ContainsKey($key)) { throw "fake chkdsk has no answer for '$key'" }
        $Table[$key]
    }.GetNewClosure()
    return @{ Invoke = $invoke; Calls = $calls }
}
function Answer([int]$exit, [string]$report) { @{ Exit = $exit; Report = $report } }

function Verdict($shape, $modes, $table) {
    $fake = New-FakeChkdsk $table
    $v = Invoke-ChkdskVerdict -Modes $modes -VerdictShape $shape -InvokeMode $fake.Invoke
    return @{ V = $v; Calls = $fake.Calls }
}

# ── Damaged ────────────────────────────────────────────────────────────

# The run that passed, 37072840058: the read-only pass finds the damaged
# mirror (3), /scan does not look at the mirror (0), /F /X repairs it (1)
# and the scan after the repair is clean.
$r = Verdict 'Damaged' @('readonly', '/scan') @{
    'readonly' = (Answer 3 $Found); '/scan' = (Answer 0 $Clean)
    '/F /X' = (Answer 1 $Fixed); '/scan-post' = (Answer 0 $Clean)
}
Check ($r.V.passed -eq $true) 'Damaged: found, repaired, clean afterwards passes'
Check ($r.Calls -notcontains '/F /X-offline-fallback') 'Damaged: no offline fallback when the scan after the repair ran'

# rust-fs-ntfs#419, run 37074606783: identical up to the scan after the
# repair, which could not take its snapshot and scanned nothing. That is not
# Windows saying the volume is still damaged, and it must not read as if it
# were: the offline check that needs no snapshot decides instead.
$r = Verdict 'Damaged' @('readonly', '/scan') @{
    'readonly' = (Answer 3 $Found); '/scan' = (Answer 0 $Clean)
    '/F /X' = (Answer 1 $Fixed); '/scan-post' = (Answer 10 $SnapshotProviderError)
    '/F /X-offline-fallback' = (Answer 0 $Clean)
}
Check ($r.V.passed -eq $true) 'Damaged (#419): a post-repair scan with no snapshot is decided by the offline check'
Check ($r.Calls -contains '/F /X-offline-fallback') 'Damaged (#419): the offline check ran'
Check ($r.V.modes -and $r.V.modes['/scan-post'].state -eq 'not-scanned') 'Damaged (#419): the scan that did not run is recorded as not-scanned'
Check ($r.V.modes -and $r.V.modes['/scan-post-offline-fallback'].state -eq 'scanned') 'Damaged (#419): the offline check is recorded beside it'

# The offline check still finding something after the repair is a failure.
$r = Verdict 'Damaged' @('readonly', '/scan') @{
    'readonly' = (Answer 3 $Found); '/scan' = (Answer 0 $Clean)
    '/F /X' = (Answer 1 $Fixed); '/scan-post' = (Answer 10 $SnapshotProviderError)
    '/F /X-offline-fallback' = (Answer 1 $Fixed)
}
Check ($r.V.passed -eq $false) 'Damaged: an offline check that repairs again after the repair fails'

# A non-zero post-repair scan that DID run is the volume's answer: no fallback.
$r = Verdict 'Damaged' @('readonly', '/scan') @{
    'readonly' = (Answer 3 $Found); '/scan' = (Answer 0 $Clean)
    '/F /X' = (Answer 1 $Fixed); '/scan-post' = (Answer 10 $Found)
}
Check ($r.V.passed -eq $false) 'Damaged: a post-repair scan that ran and found problems fails'
Check ($r.Calls -notcontains '/F /X-offline-fallback') 'Damaged: no fallback replaces a scan that ran'

# A scan that could not run found nothing, so it cannot be the evidence that
# Windows saw the damage.
$r = Verdict 'Damaged' @('readonly', '/scan') @{
    'readonly' = (Answer 0 $Clean); '/scan' = (Answer 10 $SnapshotProviderError)
    '/F /X' = (Answer 0 $Clean); '/scan-post' = (Answer 0 $Clean)
}
Check ($r.V.passed -eq $false) 'Damaged: a scan with no snapshot is not damage found'
Check ($r.V.damage_found -eq $false) 'Damaged: damage_found stays false when only a snapshot failed'

# ── RepairRequired ─────────────────────────────────────────────────────

# cli-set-dirty-fsck-win-dirty-chkdsk, run 37074606783: /scan 13 on the dirty
# volume, /F /X 0, clean afterwards.
$r = Verdict 'RepairRequired' @('readonly', '/scan') @{
    'readonly' = (Answer 0 $Clean); '/scan' = (Answer 13 $Found)
    '/F /X' = (Answer 0 $Clean); '/scan-post' = (Answer 0 $Clean)
}
Check ($r.V.passed -eq $true) 'RepairRequired: dirty, fixed, clean afterwards passes'

$r = Verdict 'RepairRequired' @('readonly', '/scan') @{
    'readonly' = (Answer 0 $Clean); '/scan' = (Answer 13 $Found)
    '/F /X' = (Answer 0 $Clean); '/scan-post' = (Answer 10 $SnapshotProviderError)
    '/F /X-offline-fallback' = (Answer 0 $Clean)
}
Check ($r.V.passed -eq $true) 'RepairRequired: a post-fix scan with no snapshot is decided by the offline check'

# The pre-fix /scan is the evidence the volume was dirty. One that did not run
# is no evidence, and nothing offline can stand in for it without repairing
# the very state it should observe.
$r = Verdict 'RepairRequired' @('readonly', '/scan') @{
    'readonly' = (Answer 0 $Clean); '/scan' = (Answer 10 $SnapshotProviderError)
    '/F /X' = (Answer 0 $Clean); '/scan-post' = (Answer 0 $Clean)
}
Check ($r.V.passed -eq $false) 'RepairRequired: a pre-fix scan with no snapshot is not evidence of dirt'

# ── Clean ──────────────────────────────────────────────────────────────

$r = Verdict 'Clean' @('readonly', '/scan') @{ 'readonly' = (Answer 0 $Clean); '/scan' = (Answer 0 $Clean) }
Check ($r.V.passed -eq $true) 'Clean: both passes clean passes'

# rust-fs-ntfs#295: /scan exit 11 because no snapshot fit on a 32 MiB volume.
# It scanned nothing, so it cannot pass on its own; the offline check decides.
$r = Verdict 'Clean' @('readonly', '/scan') @{
    'readonly' = (Answer 0 $Clean); '/scan' = (Answer 11 $SnapshotNoStorage)
    '/F /X-offline-fallback' = (Answer 0 $Clean)
}
Check ($r.V.passed -eq $true) 'Clean (#295): no snapshot, offline check clean passes'
Check ($r.V.modes -and $r.V.modes['/scan'].state -eq 'not-scanned') 'Clean (#295): the scan is recorded as not-scanned'

$r = Verdict 'Clean' @('readonly', '/scan') @{
    'readonly' = (Answer 0 $Clean); '/scan' = (Answer 11 $SnapshotNoStorage)
    '/F /X-offline-fallback' = (Answer 1 $Fixed)
}
Check ($r.V.passed -eq $false) 'Clean (#295): no snapshot, offline check repairing something fails'

# Exit 11 from a scan that ran is the accepted frs.cxx 60f ceiling: unchanged.
$r = Verdict 'Clean' @('readonly', '/scan') @{ 'readonly' = (Answer 0 $Clean); '/scan' = (Answer 11 $Found) }
Check ($r.V.passed -eq $true) 'Clean: /scan exit 11 from a scan that ran is still accepted'
Check ($r.Calls -notcontains '/F /X-offline-fallback') 'Clean: no fallback for a scan that ran'

$r = Verdict 'Clean' @('readonly', '/scan') @{ 'readonly' = (Answer 3 $Found); '/scan' = (Answer 0 $Clean) }
Check ($r.V.passed -eq $false) 'Clean: the read-only pass finding problems fails'

# rust-fs-ntfs#429: chkdsk prints the shadow-copy warning as its first line
# and then scans the live volume. A /scan that did that and found errors ran;
# only the outcome line "A snapshot error occured while scanning this drive"
# says it did not.
$r = Verdict 'Clean' @('readonly', '/scan') @{
    'readonly' = (Answer 0 $Clean); '/scan' = (Answer 3 $ShadowWarnedFound)
}
Check ($r.V.modes -and $r.V.modes['/scan'].state -eq 'failed') 'Clean (#429): a scan that warned about shadow copy storage and found errors is recorded as failed'
Check ($r.V.passed -eq $false) 'Clean (#429): ... and fails the verdict'
Check ($r.Calls -notcontains '/F /X-offline-fallback') 'Clean (#429): ... with no offline fallback standing in for it'

# ── the record ─────────────────────────────────────────────────────────

# matrix-fetch-diag.sh reads modes.<mode>.{exit,state,reason} out of
# verdict.json, and says "no per-mode states" for a verdict without them.
$r = Verdict 'Damaged' @('readonly', '/scan') @{
    'readonly' = (Answer 3 $Found); '/scan' = (Answer 0 $Clean)
    '/F /X' = (Answer 1 $Fixed); '/scan-post' = (Answer 10 $SnapshotProviderError)
    '/F /X-offline-fallback' = (Answer 0 $Clean)
}
$json = $r.V | ConvertTo-Json -Depth 5 -Compress | ConvertFrom-Json
$bad = @()
if (-not $json.modes) {
    $bad += 'no modes'
} else {
    foreach ($p in $json.modes.PSObject.Properties) {
        if ($p.Value.state -notin 'scanned', 'not-scanned', 'failed') { $bad += "$($p.Name) state" }
        if ($p.Value.exit -isnot [long] -and $p.Value.exit -isnot [int]) { $bad += "$($p.Name) exit" }
        if (-not $p.Value.reason) { $bad += "$($p.Name) reason" }
    }
}
Check ($bad.Count -eq 0) "record: every mode carries exit, state and reason ($($bad -join ', '))"
Check ($json.exits.'/scan-post' -eq 10) 'record: the raw exit codes are still there'

# rust-fs-ntfs#430: the reader of verdict.json (matrix-fetch-diag.sh) was
# moved to the per-mode record while the writer still wrote the old one, and
# nothing compared the two. tests/scripts/fixtures/chkdsk-verdict/ holds the
# exact record this writer emits for three runs; this pins the writer to
# them, and tests/scripts/matrix-fetch-diag.sh feeds the same files to the
# reader. Each side is compared after one JSON round trip in this shell, so
# the two PowerShells' formatting differences cancel out.
function Same-Record($verdict, [string]$name) {
    $file = Join-Path $PSScriptRoot "fixtures/chkdsk-verdict/$name.json"
    $want = Get-Content -LiteralPath $file -Raw | ConvertFrom-Json | ConvertTo-Json -Depth 5 -Compress
    $got = $verdict | ConvertTo-Json -Depth 5 -Compress | ConvertFrom-Json | ConvertTo-Json -Depth 5 -Compress
    if ($got -ne $want) { Write-Host "     got:  $got"; Write-Host "     want: $want" }
    return $got -eq $want
}
$r = Verdict 'Clean' @('readonly', '/scan') @{ 'readonly' = (Answer 0 $Clean); '/scan' = (Answer 0 $Clean) }
Check (Same-Record $r.V 'clean') 'record (#430): a clean pass writes fixtures/chkdsk-verdict/clean.json'
$r = Verdict 'Clean' @('readonly', '/scan') @{
    'readonly' = (Answer 0 $Clean); '/scan' = (Answer 11 $SnapshotNoStorage)
    '/F /X-offline-fallback' = (Answer 0 $Clean)
}
Check (Same-Record $r.V 'clean-no-snapshot') 'record (#430): a clean pass with no snapshot writes fixtures/chkdsk-verdict/clean-no-snapshot.json'
$r = Verdict 'Damaged' @('readonly', '/scan') @{
    'readonly' = (Answer 3 $Found); '/scan' = (Answer 0 $Clean)
    '/F /X' = (Answer 1 $Fixed); '/scan-post' = (Answer 0 $Clean)
}
Check (Same-Record $r.V 'damaged') 'record (#430): a damaged pass writes fixtures/chkdsk-verdict/damaged.json'

if ($script:fails -gt 0) {
    Write-Host "chkdsk-verdict: $($script:fails) check(s) failed"
    exit 1
}
Write-Host 'chkdsk-verdict: all checks passed'
