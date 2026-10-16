# Records what FSCTL_QUERY_USN_JOURNAL reports for an active change journal
# (#609).
#
# The runner's own volumes are never queried or touched: their journals may be
# inactive (fsutil answers "Error 1179: The volume change journal is not
# active"), and a system journal is not ours to create or reset. This script
# formats a disposable NTFS volume (a VHD attached from an image in the
# runner's temp directory, the same lifecycle capture-usn-v2-records.ps1
# uses), creates a change journal on it with `fsutil usn createjournal`, and
# reads it with fsutil usn queryjournal, which issues FSCTL_QUERY_USN_JOURNAL
# and prints the USN_JOURNAL_DATA fields. probe-usn-read-cursors.ps1 then runs
# against the same volume, still attached, so both outputs describe one
# journal. The VHD is detached and both images are removed on every path,
# whether the capture or the probe failed. Values are written as fsutil prints them, in hex,
# so the 64-bit journal ID reaches the test without passing through a double,
# and fsutil's raw output is printed to the log beside them.
#
# Output: test-disks/usn-journal-data.tsv, one `field<TAB>hex` row per field,
# and the probe's test-disks/usn-read-probe.tsv.
#
# Run from the repository root on an elevated Windows host. rust-img-vhd must be
# on PATH; CI installs the pinned rust-img-vhd release before invoking this.
$ErrorActionPreference = 'Stop'

. "$PSScriptRoot\..\scripts\fs-windows-test-harness\_lib.ps1"

$tempRoot = if ($env:RUNNER_TEMP) { $env:RUNNER_TEMP } else { [System.IO.Path]::GetTempPath() }
$tag = [guid]::NewGuid().ToString('N').Substring(0, 8)
$image = Join-Path $tempRoot "usn-journal-data-$tag.img"
$diag = Join-Path $tempRoot "usn-journal-data-$tag"
$out = [System.IO.Path]::GetFullPath('test-disks/usn-journal-data.tsv')
New-Item -ItemType Directory -Path $diag -Force | Out-Null
$stream = [System.IO.File]::Create($image)
try { $stream.SetLength(256MB) } finally { $stream.Close() }

$state = $null
try {
    $state = Initialize-VhdFromImg -ImagePath $image -Diag $diag
    $letter = Mount-VhdAndGetLetter -Vhd $state.Vhd
    Format-Volume -DriveLetter $letter -FileSystem NTFS -NewFileSystemLabel 'USNJOURNAL' `
        -Force -Confirm:$false | Out-Null
    $volume = "${letter}:"
    $o = & fsutil.exe usn createjournal m=8388608 a=1048576 $volume 2>&1
    if ($LASTEXITCODE -ne 0) { throw "fsutil usn createjournal on $volume failed: $o" }
    $lines = & fsutil.exe usn queryjournal $volume 2>&1
    if ($LASTEXITCODE -ne 0) { throw "fsutil usn queryjournal $volume failed: $lines" }
    Write-Host "fsutil usn queryjournal $volume (disposable volume) printed:"
    $lines | ForEach-Object { Write-Host "  $_" }

    $wanted = @{
        'Usn Journal ID'   = 'journal_id'
        'First Usn'        = 'first_usn'
        'Next Usn'         = 'next_usn'
        'Lowest Valid Usn' = 'lowest_valid_usn'
        'Max Usn'          = 'max_usn'
        'Maximum Size'     = 'maximum_size'
        'Allocation Delta' = 'allocation_delta'
    }
    $rows = @("# fsutil usn queryjournal $volume")
    $plainField = '^\s*([^:]+?)\s*:\s*(0x[0-9a-fA-F]+)\s*$'
    # fsutil appends human-readable units to its two size fields. Keep the
    # hexadecimal value authoritative and require bare hex for other fields.
    $sizeField = '^\s*(Maximum Size|Allocation Delta)\s*:\s*(0x[0-9a-fA-F]+)\s+\(\s*[0-9]+(?:\.[0-9]+)?\s+[KMGTPE]?B\)\s*$'
    foreach ($line in $lines) {
        if ((("$line" -match $plainField) -or ("$line" -match $sizeField)) -and $wanted.ContainsKey($Matches[1])) {
            $rows += "$($wanted[$Matches[1]])`t$($Matches[2])"
        }
    }
    if ($rows.Count -ne 8) { throw "expected 7 USN_JOURNAL_DATA fields, parsed $($rows.Count - 1): $lines" }
    Set-Content -LiteralPath $out -Value $rows
    Get-Content -LiteralPath $out

    # The read probe runs on this same journal before the volume goes away.
    $controls = [System.IO.Path]::GetFullPath('test-disks/usn-journal-identity-controls.txt')
    $verdicts = @("# journal identity controls on $volume (disposable volume)")
    $origSha = (Get-FileHash -Algorithm SHA256 -LiteralPath $out).Hash
    try {
        & "$PSScriptRoot\probe-usn-read-cursors.ps1" -Volume $volume
        $verdicts += "positive_control`tPASS`tthe probe read the journal the capture recorded"
    } catch {
        $verdicts += "positive_control`tFAIL`t$($_.Exception.Message)"
        Set-Content -LiteralPath $controls -Value $verdicts
        throw
    }

    # Negative control: the same probe on the same volume, given a copy of the
    # capture metadata whose journal_id differs in its top bit, must fail with
    # the journal-identity mismatch and nothing else.
    $capturedId = Get-Content -LiteralPath $out |
        Where-Object { $_ -match '^journal_id\t(0x[0-9a-fA-F]+)$' } |
        ForEach-Object { [Convert]::ToUInt64($Matches[1].Substring(2), 16) } |
        Select-Object -First 1
    $wrongId = $capturedId -bxor [uint64]0x8000000000000000
    $controlTsv = [System.IO.Path]::GetFullPath('test-disks/usn-journal-data.wrong-id-control.tsv')
    $controlProbe = [System.IO.Path]::GetFullPath('test-disks/usn-read-probe.wrong-id-control.tsv')
    Get-Content -LiteralPath $out | ForEach-Object {
        if ($_ -match '^journal_id\t') { "journal_id`t0x{0:x16}" -f $wrongId } else { $_ }
    } | Set-Content -LiteralPath $controlTsv
    $verdicts += ("negative_control_ids`tcaptured 0x{0:x16}`tcontrol 0x{1:x16}" -f $capturedId, $wrongId)
    $failure = $null
    try {
        & "$PSScriptRoot\probe-usn-read-cursors.ps1" -Volume $volume `
            -JournalCapture $controlTsv -Out $controlProbe
    } catch {
        $failure = $_.Exception.Message
    }
    $expected = ('recorded 0x{0:x16}: not the captured journal' -f $wrongId)
    $actualRow = if (Test-Path -LiteralPath $controlProbe) {
        Get-Content -LiteralPath $controlProbe | Where-Object { $_ -match '^journal\tjournal_id\t' }
    }
    $negative = if ($null -eq $failure) {
        'FAIL - the probe accepted a journal_id the volume does not carry'
    } elseif (-not $failure.EndsWith($expected)) {
        "FAIL - the probe failed for another reason: $failure"
    } elseif ($actualRow -ne ("journal`tjournal_id`t0x{0:x16}" -f $capturedId)) {
        "FAIL - the control manifest does not record the actual journal: '$actualRow'"
    } else {
        "PASS - $failure"
    }
    $verdicts += "negative_control`t$negative"
    if ((Get-FileHash -Algorithm SHA256 -LiteralPath $out).Hash -ne $origSha) {
        $verdicts += "original_capture`tFAIL - $out changed during the controls"
    } else {
        $verdicts += "original_capture`tUNCHANGED`tsha256 $origSha"
    }
    Set-Content -LiteralPath $controls -Value $verdicts
    Get-Content -LiteralPath $controls
    $ran = @($verdicts | Where-Object { $_ -match '^(positive_control|negative_control|original_capture)\t' })
    if ($ran.Count -ne 3 -or ($verdicts | Where-Object { $_ -match '\tFAIL' })) {
        throw "journal identity controls did not all pass: see $controls"
    }
} finally {
    if ($state) {
        Get-DiskImage -ImagePath $state.Vhd -ErrorAction SilentlyContinue |
            Where-Object Attached |
            Dismount-DiskImage -ErrorAction SilentlyContinue | Out-Null
        Remove-Item -LiteralPath $state.Vhd -Force -ErrorAction SilentlyContinue
    }
    Remove-Item -LiteralPath $image -Force -ErrorAction SilentlyContinue
}
