# CI-only controls: real Windows VHDs, with a diskpart command shim that
# injects genuine native failures. Child processes use Actions' exit wrapper.
param([string]$Case, [string]$CaptureScript, [string]$OutDir = 'tmp/hardlink-oracle/lifecycle')
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false

if ($Case) {
    # Global, not script, scope: a script-scope modifier resolves at run time
    # to the nearest script on the call stack, which is the capture script
    # calling this shim, so script-scoped counts never reached the finally
    # block below.
    $global:detaches = 0
    $global:injected = $false
    function diskpart {
        param([string]$s, [string]$file)
        $commands = @(Get-Content -LiteralPath $file)
        $detach = $commands -contains 'detach vdisk'
        if ($detach) { $global:detaches++ }
        $inject = -not $global:injected -and (
            ($Case -eq 'attachment-error' -and $commands -contains 'attach vdisk') -or
            ($Case -eq 'initial-detach-error' -and $detach) -or
            ($Case -eq 'partial-capture-error' -and $global:detaches -eq 2)
        )
        if ($inject -or ($Case -eq 'cleanup-error' -and $detach)) {
            $global:injected = $true
            # Fail in diskpart itself, rather than manufacturing LASTEXITCODE.
            $bad = 'select vdisk file="' + (Join-Path $OutDir 'nonexistent.vhd') + '"'
            $commands = @($commands | ForEach-Object {
                if ($_ -eq 'attach vdisk' -or $_ -eq 'detach vdisk') { $bad } else { $_ }
            })
            Set-Content -LiteralPath $file -Value $commands -Encoding ascii
        }
        & "$env:SystemRoot\System32\diskpart.exe" /s $file
    }
    try {
        & $CaptureScript -OutDir $OutDir
    } finally {
        Set-Content -LiteralPath (Join-Path $OutDir 'detach-count.txt') -Value $global:detaches
    }
    # Same trailer as Actions' pwsh shell. In particular, don't hide the
    # swallowed duplicate-detach error in the historical capture.
    if (Test-Path variable:\LASTEXITCODE) { exit $LASTEXITCODE }
    return
}

$root = [System.IO.Path]::GetFullPath($OutDir)
if (Test-Path -LiteralPath $root) { throw "control directory already exists: $root" }
New-Item -ItemType Directory -Path $root | Out-Null
$current = (Resolve-Path (Join-Path $PSScriptRoot 'hardlink-readonly-index-capture.ps1')).Path
$legacy = Join-Path $root 'legacy.ps1'
# The capture script as it stood at the completed red, kept in the tree as
# its own file: a commit of this branch would not survive a rebase or the
# squash merge. The blob id proves it is the recorded script, byte for byte.
$legacyPath = 'tests/scripts/hardlink-readonly-index-capture.legacy.ps1'
$legacyBlob = 'b098f71cc472ac6f102120fcf1904b8a7f8bb7b3'
$legacyFound = (git rev-parse "HEAD:$legacyPath").Trim()
if ($LASTEXITCODE -ne 0 -or $legacyFound -ne $legacyBlob) {
    throw "the completed-red source is not the recorded capture script: $legacyPath is $legacyFound, not $legacyBlob"
}
git cat-file blob $legacyBlob | Set-Content -LiteralPath $legacy -Encoding ascii
if ($LASTEXITCODE -ne 0) { throw 'the completed-red source is unavailable' }
$head = (git rev-parse HEAD).Trim()
if ($LASTEXITCODE -ne 0) { throw 'cannot identify lifecycle control source' }
Set-Content -LiteralPath (Join-Path $root 'source.tsv') -Value @(
    "legacy_source`t${legacyPath}@$legacyBlob",
    "current_source`t$head",
    "workflow_sha`t$env:GITHUB_SHA"
)
$results = [System.Collections.Generic.List[string]]::new()
$results.Add("case`texit`tdetaches`timages")
$stages = @('s0-created', 's1-readonly', 's2-reopen', 's3-rename')
foreach ($name in @('legacy', 'success', 'attachment-error', 'initial-detach-error', 'partial-capture-error', 'cleanup-error')) {
    $dir = Join-Path $root $name
    $source = if ($name -eq 'legacy') { $legacy } else { $current }
    try {
        # Process.ExitCode keeps expected negative controls out of this
        # process's LASTEXITCODE; no status reset or blanket success exit.
        $start = [System.Diagnostics.ProcessStartInfo]::new((Get-Process -Id $PID).Path)
        $start.UseShellExecute = $false
        $start.RedirectStandardOutput = $true
        $start.RedirectStandardError = $true
        # As Actions' pwsh shell runs a step: -command ". '<script>'". In that
        # mode a script's exit code other than 0 or 1 reaches the process as
        # 1, which is the status the completed red recorded; -File would pass
        # diskpart's raw HRESULT through instead.
        $quote = { param([string]$text) "'" + $text.Replace("'", "''") + "'" }
        $command = '. ' + (& $quote $PSCommandPath) + ' -Case ' + (& $quote $name) +
            ' -CaptureScript ' + (& $quote $source) + ' -OutDir ' + (& $quote $dir)
        foreach ($arg in @('-NoLogo', '-NoProfile', '-Command', $command)) {
            $start.ArgumentList.Add($arg)
        }
        $process = [System.Diagnostics.Process]::Start($start)
        try {
            $stdout = $process.StandardOutput.ReadToEndAsync()
            $stderr = $process.StandardError.ReadToEndAsync()
            $process.WaitForExit()
            $status = $process.ExitCode
            Set-Content -LiteralPath (Join-Path $root "$name.log") -Value ($stdout.Result + $stderr.Result)
        } finally { $process.Dispose() }
        $count = [int](Get-Content -LiteralPath (Join-Path $dir 'detach-count.txt'))
        $images = @($stages | Where-Object { Test-Path -LiteralPath (Join-Path $dir "$_.img") })
        $results.Add("$name`t$status`t$count`t$($images.Count)")
        if ($name -in @('legacy', 'success')) {
            $wantExit = if ($name -eq 'legacy') { 1 } else { 0 }
            $wantDetaches = if ($name -eq 'legacy') { 6 } else { 5 }
            if ($status -ne $wantExit -or $count -ne $wantDetaches -or $images.Count -ne 4) {
                throw "$name expected exit=$wantExit detaches=$wantDetaches images=4; got $status/$count/$($images.Count)"
            }
            $rows = @(Get-Content -LiteralPath (Join-Path $dir 'manifest.tsv'))
            $imageRows = @($rows | Where-Object { $_.StartsWith("image`t") })
            if ($imageRows.Count -ne 4) { throw "$name did not complete all stages" }
            foreach ($row in $imageRows) {
                $fields = $row.Split("`t")
                $hash = (Get-FileHash -Algorithm SHA256 -LiteralPath (Join-Path $dir $fields[2])).Hash.ToLowerInvariant()
                if ($hash -ne $fields[3]) { throw "$name image hash mismatch: $row" }
            }
            if ($name -eq 'success') {
                $head = (git rev-parse HEAD).Trim()
                if ($LASTEXITCODE -ne 0 -or $rows -notcontains "head`t$head") { throw 'source head is not bound to the checkout' }
            }
        } else {
            if ($status -eq 0) { throw "$name incorrectly succeeded" }
            $log = Get-Content -Raw -LiteralPath (Join-Path $root "$name.log")
            # diskpart exits with an HRESULT, which is negative as a signed
            # status (0x80070057 is -2147024809); any nonzero status counts.
            if ($log -notmatch 'diskpart failed \(-?[1-9][0-9]*\)') { throw "$name lost the genuine native error: $log" }
            if ($name -eq 'cleanup-error' -and ($log -notmatch 'capture failed:' -or $log -notmatch 'cleanup failed:')) {
                throw 'the cleanup control lost either its primary or cleanup error'
            }
            if (Test-Path -LiteralPath (Join-Path $dir 'manifest.tsv')) { throw "$name published an incomplete manifest" }
            $wantImages = if ($name -eq 'partial-capture-error') { 1 } else { 0 }
            if ($images.Count -ne $wantImages) { throw "$name expected $wantImages partial images, got $($images.Count)" }
        }
        $vhd = Join-Path $dir 'hardlink-oracle.vhd'
        if ($name -eq 'cleanup-error') {
            if (-not (Test-Path -LiteralPath $vhd)) { throw 'failed cleanup concealed ownership of the VHD' }
        } elseif (Test-Path -LiteralPath $vhd) { throw "$name leaked its VHD" }
    } finally {
        # Only these controls' named VHD and four stage files. Retain logs
        # and manifests even when a control fails, and report cleanup errors.
        $vhd = Join-Path $dir 'hardlink-oracle.vhd'
        if (Test-Path -LiteralPath $vhd) {
            if ((Get-DiskImage -ImagePath $vhd -ErrorAction Stop).Attached) {
                Dismount-DiskImage -ImagePath $vhd -ErrorAction Stop | Out-Null
            }
            Remove-Item -LiteralPath $vhd -Force
        }
        foreach ($stage in $stages) {
            $image = Join-Path $dir "$stage.img"
            if (Test-Path -LiteralPath $image) { Remove-Item -LiteralPath $image -Force }
        }
        Set-Content -LiteralPath (Join-Path $root 'results.tsv') -Value $results
    }
}
Write-Host 'hardlink-capture-lifecycle-controls: six controls passed (legacy red, current green, four native failures)'
