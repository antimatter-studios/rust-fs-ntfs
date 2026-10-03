# _chkdsk-verdict.ps1 -- what a set of chkdsk passes says about a volume.
#
# Dot-sourced by win-chkdsk.ps1. Nothing here mounts anything or runs chkdsk
# itself: each pass goes through the -InvokeMode scriptblock the caller
# passes, which returns @{ Exit = <int>; Report = <string> }. That is what
# lets tests/scripts/chkdsk-verdict.ps1 drive the verdict with chkdsk's real
# reports on any host that has PowerShell, with no VM.
#
# A /scan THAT COULD NOT TAKE ITS SNAPSHOT SCANNED NOTHING (#419, #295).
# `chkdsk /scan` is an online scan: it reads a volume shadow copy, not the
# volume. When the copy cannot be made it says so and exits non-zero --
#
#   The shadow copy provider had an error. ...
#   A snapshot error occured while scanning this drive. You can try again,
#   but if this problem persists, run an offline scan and fix.
#
# -- exit 10 on the hosted runner (runs 36734098054 and 37074606783, each on
# the scan after a repair, each passing on the same code elsewhere), exit 11
# on a volume too small to hold the copy (#295). The exit code is shared
# with real findings, so only the report can tell the two apart. Read as a
# finding it made `main` red at random, and it let a scan that never ran
# count as "Windows found the damage". Such a pass is recorded as
# `not-scanned`, and where the verdict needs it, the offline check chkdsk
# itself recommends -- `/F /X`, which dismounts the volume and needs no
# snapshot -- runs in its place and is recorded beside it. `/F` that has
# nothing to fix exits 0, so on a volume that should be clean it is a check,
# not a repair: any other exit fails the verdict.
#
# Every pass lands in `modes`, as `{ exit, state, reason }` with state one of
# scanned / not-scanned / failed, which is what scripts/matrix-fetch-diag.sh
# reads. `exits` keeps the bare exit codes beside it.

function Test-ChkdskSnapshotFailure {
    param([AllowEmptyString()] [AllowNull()] [string]$Report)
    return [bool]($Report -match '(?i)snapshot\s+error|shadow\s+copy')
}

# One pass, as a record. $Accepted are the non-zero exits this verdict shape
# takes as a pass that ran and is acceptable (Clean's /scan: 11, the known
# frs.cxx 60f ceiling, and 13).
function New-ChkdskModeResult {
    param(
        [Parameter(Mandatory=$true)] [string]$Mode,
        [Parameter(Mandatory=$true)] [int]$Exit,
        [AllowEmptyString()] [AllowNull()] [string]$Report = '',
        [int[]]$Accepted = @()
    )
    if ($Exit -eq 0) {
        return [ordered]@{ exit = $Exit; state = 'scanned'; reason = 'chkdsk found nothing to report' }
    }
    if ($Exit -in $Accepted) {
        return [ordered]@{ exit = $Exit; state = 'scanned'; reason = "exit $Exit accepted for this verdict shape" }
    }
    return [ordered]@{ exit = $Exit; state = 'failed'; reason = "chkdsk exited $Exit" }
}

function Invoke-ChkdskVerdict {
    param(
        [Parameter(Mandatory=$true)] [AllowEmptyCollection()] [string[]]$Modes,
        [Parameter(Mandatory=$true)] [ValidateSet('Clean', 'RepairRequired', 'Damaged')] [string]$VerdictShape,
        [Parameter(Mandatory=$true)] [scriptblock]$InvokeMode
    )

    $modeResults = [ordered]@{}
    $rawExits = [ordered]@{}

    # Run one pass and record it under $key.
    $run = {
        param([string]$mode, [string]$suffix, [string]$key, [int[]]$accepted = @())
        $answer = & $InvokeMode $mode $suffix
        $result = New-ChkdskModeResult -Mode $mode -Exit $answer.Exit -Report $answer.Report -Accepted $accepted
        $modeResults[$key] = $result
        $rawExits[$key] = $result.exit
        return $result
    }
    # A /scan that did not run, replaced by the offline check. True when the
    # pass (or its stand-in) says the volume is clean.
    $cleanOrOffline = {
        param($result, [string]$key)
        if ($result.state -eq 'scanned') { return $true }
        if ($result.state -ne 'not-scanned') { return $false }
        $fallback = & $run '/F /X' '-offline-fallback' "$key-offline-fallback"
        if ($fallback.state -eq 'scanned') {
            $fallback.reason = 'offline /F /X found nothing to fix, in place of the online scan'
            return $true
        }
        $fallback.reason = "offline /F /X, in place of the online scan, exited $($fallback.exit)"
        return $false
    }

    if ($VerdictShape -eq 'Clean') {
        $passed = $true
        foreach ($mode in $Modes) {
            $accepted = if ($mode -eq 'readonly') { @() } else { @(11, 13) }
            $result = & $run $mode '' $mode $accepted
            if (-not (& $cleanOrOffline $result $mode)) { $passed = $false }
        }
        return [ordered]@{
            passed = $passed
            verdict_shape = 'clean'
            modes = $modeResults
            exits = $rawExits
        }
    }

    if ($VerdictShape -eq 'Damaged') {
        # Windows must say it found the damage: some pass that ran and
        # reported. `/scan` exits 0 on a damaged $MFTMirr (it does not
        # compare it), the read-only pass exits 3, so no single mode is
        # required -- but a pass that did not run found nothing.
        $found = $false
        foreach ($mode in $Modes) {
            $result = & $run $mode '' $mode
            if ($result.state -eq 'failed') { $found = $true }
        }
        # 0 or 1: 1 is "errors found and fixed", the expected answer here.
        $fix = & $run '/F /X' '' '/F /X' @(1)
        $post = & $run '/scan' '-post' '/scan-post'
        $postClean = & $cleanOrOffline $post '/scan-post'
        return [ordered]@{
            passed = $found -and $fix.state -eq 'scanned' -and $postClean
            verdict_shape = 'damaged'
            modes = $modeResults
            exits = $rawExits
            damage_found = $found
            fix_exit = $fix.exit
            post_scan_exit = $post.exit
        }
    }

    # RepairRequired: the pre-fix /scan is the evidence the volume needed
    # repair. One that did not run is no evidence, and no offline check can
    # stand in for it without repairing the state it was to observe.
    $preScan = $null
    foreach ($mode in $Modes) {
        $result = & $run $mode '' $mode
        if ($mode -eq '/scan') { $preScan = $result }
    }
    $fix = & $run '/F /X' '' '/F /X'
    $post = & $run '/scan' '-post' '/scan-post'
    $postClean = & $cleanOrOffline $post '/scan-post'
    $preScanExit = $null
    if ($null -ne $preScan) { $preScanExit = $preScan.exit }
    return [ordered]@{
        passed = ($null -ne $preScan) -and $preScan.state -eq 'failed' `
                 -and $fix.state -eq 'scanned' -and $postClean
        verdict_shape = 'repair-required'
        modes = $modeResults
        exits = $rawExits
        pre_scan_exit = $preScanExit
        fix_exit = $fix.exit
        post_scan_exit = $post.exit
    }
}
