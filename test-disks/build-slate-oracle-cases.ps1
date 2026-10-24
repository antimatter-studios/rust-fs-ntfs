# build-slate-oracle-cases.ps1 -- the Windows half of the Slate checker
# oracle (#687): formatted volumes, each graded by Windows chkdsk.
#
#   pwsh -File test-disks/build-slate-oracle-cases.ps1 -Cli target/release/rust-fs-ntfs.exe
#
# Four 256 MiB volumes, two from each formatter at 4 KiB and 64 KiB
# clusters:
#
#   win-4k, win-64k    Windows' own formatter (Format-Volume, through
#                      New-WindowsFixture). These are the controls: Slate has
#                      to accept what Windows makes before its verdict on
#                      anything else means much.
#   ours-4k, ours-64k  this crate's mkfs.ntfs.
#
# Each image is graded read-only by win-chkdsk.ps1, the matrix's own chkdsk
# step, on a scratch copy: win-chkdsk mounts the copy and syncs it back after
# Windows has mounted it, so grading the original would change the bytes
# Slate is then given. The originals are left in -OutDir/images untouched,
# and -OutDir/cases.tsv records for each one its SHA-256 and Windows'
# verdict. scripts/slate-oracle.sh compare reads that table on Linux.
#
# New-WindowsFixture formats the whole scratch partition, which is the
# 256 MiB image plus the wrapper's slack (about 304 MiB), and then keeps
# only the image's first 256 MiB: its boot sector claims more sectors than
# the image holds and its backup boot sector is cut off, so a checker that
# reads the geometry cannot judge it at all (#836). The Windows controls
# therefore shrink their freshly formatted volume to 256 MiB with Windows'
# own Resize-Partition before the capture, and every image, from either
# formatter, must hold the whole volume its boot sector describes.
#
# A case that cannot be graded at all -- no image, no chkdsk record -- stops
# the run. A case Windows grades and does not accept is recorded as
# `not-clean` and left for the comparison to report.
#
# With -SlateWrites and -SlateWriteImages it formats nothing and grades the
# copies Slate's copy-only writer left (#864): the writer's slate-writes.tsv
# and its images/<case>.img. Each copy is graded read-only by chkdsk first,
# and only a copy Windows accepts has its read manifest captured, so a
# volume Windows rejects is never mounted. -OutDir/slate-writes-graded.tsv
# holds one row per writer row: `accepted`, `slate-defect` (chkdsk rejected
# the copy: a Slate writer defect, kept with its chkdsk log, named in the
# job summary and never used to grade our reader; it does not fail the run)
# or `not-written` (the writer left no copy; its own step already failed).
#
# Run from the repository root on an elevated Windows host with rust-img-vhd
# on PATH; CI installs the pinned rust-img-vhd before invoking this.
param(
    [string]$Cli,
    [string]$OutDir = 'tmp/slate-oracle',
    [string]$WriterCases,
    [string]$WriterImages,
    [string]$SlateWrites,
    [string]$SlateWriteImages
)

$ErrorActionPreference = 'Stop'

. "$PSScriptRoot\..\scripts\fs-windows-test-harness\_lib.ps1"
. "$PSScriptRoot\windows-fixture.ps1"
. "$PSScriptRoot\capture-slate-windows-manifest.ps1"
. "$PSScriptRoot\capture-slate-windows-allocation.ps1"

if ([bool]$SlateWrites -ne [bool]$SlateWriteImages) {
    throw 'SlateWrites and SlateWriteImages must be supplied together'
}
if ($SlateWrites -and ($Cli -or $WriterCases)) {
    throw 'SlateWrites grades Slate-written copies only; it formats no cases'
}
if (-not $SlateWrites -and -not $Cli) { throw 'Cli is required to format the cases' }
$cli = if ($Cli) { (Resolve-Path -LiteralPath $Cli).Path }
$winChkdsk = (Resolve-Path "$PSScriptRoot\..\scripts\fs-windows-test-harness\win-chkdsk.ps1").Path
$out = [System.IO.Path]::GetFullPath($OutDir)
$images = Join-Path $out 'images'
$scratch = Join-Path $out 'scratch'
$manifests = Join-Path $out 'windows-read'
$allocations = Join-Path $out 'windows-alloc'
New-Item -ItemType Directory -Force -Path $images, $scratch, $manifests, $allocations | Out-Null

function Get-Sha256([string]$Path) {
    (Get-FileHash -Algorithm SHA256 -LiteralPath $Path).Hash.ToLowerInvariant()
}

# The bytes the boot sector says the volume spans: total sectors plus the
# backup boot sector that follows them, at the bytes-per-sector it names.
function Get-NtfsGeometryBytes([string]$Path) {
    $boot = New-Object byte[] 512
    $stream = [System.IO.File]::OpenRead($Path)
    try {
        if ($stream.Read($boot, 0, 512) -ne 512) { throw "$Path is shorter than a boot sector" }
    } finally { $stream.Close() }
    $oem = [System.Text.Encoding]::ASCII.GetString($boot, 3, 8)
    if ($oem -ne 'NTFS    ') { throw "$Path has no NTFS boot sector (OEM '$oem')" }
    $bytesPerSector = [BitConverter]::ToUInt16($boot, 11)
    $totalSectors = [BitConverter]::ToUInt64($boot, 40)
    ($totalSectors + 1) * $bytesPerSector
}

$cases = @(
    @{ Name = 'win-4k';   Origin = 'windows'; Cluster = 4096 },
    @{ Name = 'win-64k';  Origin = 'windows'; Cluster = 65536 },
    @{ Name = 'ours-4k';  Origin = 'ours';    Cluster = 4096 },
    @{ Name = 'ours-64k'; Origin = 'ours';    Cluster = 65536 }
)
if ($SlateWrites) { $cases = @() }

foreach ($case in $cases) {
    $image = Join-Path $images "$($case.Name).img"
    $label = ($case.Name -replace '-', '').ToUpperInvariant()
    if ($case.Origin -eq 'windows') {
        New-WindowsFixture -ImagePath $image -Label $label `
            -AllocationUnitSize $case.Cluster -Populate {
                param($root)
                Resize-Partition -DriveLetter $root.Substring(0, 1) -Size 256MB -ErrorAction Stop
            }
    } else {
        Remove-Item -LiteralPath $image -Force -ErrorAction SilentlyContinue
        & $cli mkfs --size 256M --label $label --cluster-size $case.Cluster $image |
            Set-Content -LiteralPath (Join-Path $out "$($case.Name).mkfs.json")
        if ($LASTEXITCODE -ne 0) { throw "mkfs.ntfs exited $LASTEXITCODE for $($case.Name)" }
    }
    if (-not (Test-Path -LiteralPath $image)) { throw "$($case.Name) left no image at $image" }
    $length = (Get-Item -LiteralPath $image).Length
    $geometry = Get-NtfsGeometryBytes $image
    if ($geometry -gt $length) {
        throw "$($case.Name) is $length bytes but its boot sector describes ${geometry}: the volume was cut short"
    }
}

# These are the actual matrix host recipes, captured before their first
# Windows step (#827). Verify the captured hash before mounting a copy;
# Windows must not silently substitute bytes for the writer's evidence.
if ([bool]$WriterCases -ne [bool]$WriterImages) {
    throw 'WriterCases and WriterImages must be supplied together'
}
if ($WriterCases) {
    $seen = @{}
    foreach ($case in $cases) { $seen[$case.Name] = $true }
    $captures = @(Import-Csv -LiteralPath $WriterCases -Delimiter "`t")
    if ($captures.Count -eq 0) { throw 'WriterCases contains no captured matrix cases' }
    foreach ($capture in $captures) {
        if ($capture.case -notmatch '^[A-Za-z0-9_-]+$' -or $seen.ContainsKey($capture.case) -or
            $capture.scenario -ne $capture.case -or $capture.image -ne "$($capture.case).img" -or
            $capture.sha256 -notmatch '^[0-9a-f]{64}$') {
            throw "Malformed or duplicate writer capture: $($capture.case)"
        }
        $seen[$capture.case] = $true
        $source = Join-Path $WriterImages $capture.image
        if ((Get-Sha256 $source) -ne $capture.sha256) {
            throw "Writer capture $($capture.case) is not the SHA-256 exported by the matrix recipe"
        }
        $image = Join-Path $images $capture.image
        Copy-Item -LiteralPath $source -Destination $image -Force
        if ((Get-Sha256 $image) -ne $capture.sha256) { throw "Copy changed writer capture $($capture.case)" }
        $cases += @{ Name = $capture.case; Origin = "writer:$($capture.scenario)" }
    }
    Copy-Item -LiteralPath $WriterCases -Destination (Join-Path $out 'writer-captures.tsv') -Force
}

# The copies Slate's copy-only writer left (#864). Each is checked against
# the SHA-256 the writer recorded before Windows sees it. A writer row that
# left no copy is still a row, so none disappears from the graded table.
$slateRows = @("case`timage`tsha256`twindows_verdict`twindows_exit")
$defects = @()
if ($SlateWrites) {
    $written = @(Import-Csv -LiteralPath $SlateWrites -Delimiter "`t")
    if ($written.Count -eq 0) { throw 'SlateWrites contains no Slate-written images' }
    $seen = @{}
    foreach ($write in $written) {
        if ($write.case -notmatch '^[A-Za-z0-9_-]+$' -or $seen.ContainsKey($write.case)) {
            throw "Malformed or duplicate Slate write: $($write.case)"
        }
        $seen[$write.case] = $true
        if ($write.result -ne 'written') {
            Write-Host "slate-write $($write.case) not-written result=$($write.result)"
            $slateRows += "$($write.case)`t-`t-`tnot-written`t-"
            continue
        }
        if ($write.written_sha256 -notmatch '^[0-9a-f]{64}$') { throw "Malformed Slate write: $($write.case)" }
        $source = Join-Path $SlateWriteImages "$($write.case).img"
        if (-not (Test-Path -LiteralPath $source)) { throw "Slate write $($write.case) left no copy at $source" }
        if ((Get-Sha256 $source) -ne $write.written_sha256) {
            throw "Slate write $($write.case) is not the SHA-256 its writer recorded"
        }
        $image = Join-Path $images "$($write.case).img"
        Copy-Item -LiteralPath $source -Destination $image -Force
        if ((Get-Sha256 $image) -ne $write.written_sha256) { throw "Copy changed Slate write $($write.case)" }
        $cases += @{ Name = $write.case; Origin = "slate:$($write.source)"; Slate = $true }
    }
}

$rows = @("case`torigin`timage`tsha256`twindows_verdict`twindows_exit")
foreach ($case in $cases) {
    $image = Join-Path $images "$($case.Name).img"
    $digest = Get-Sha256 $image
    $copy = Join-Path $scratch "$($case.Name).img"
    Copy-Item -LiteralPath $image -Destination $copy -Force
    $diag = Join-Path $out "chkdsk\$($case.Name)"

    # A child process: win-chkdsk ends with `exit`, and its status is the
    # verdict under -VerdictShape Clean.
    & pwsh -NoProfile -File $winChkdsk -ImagePath $copy -Modes readonly -Diag $diag
    $passed = ($LASTEXITCODE -eq 0)
    $exitFile = Join-Path $diag 'chkdsk-readonly-exit.txt'
    if (-not (Test-Path -LiteralPath $exitFile)) {
        throw "chkdsk left no exit record for $($case.Name) in $diag (win-chkdsk exit $LASTEXITCODE)"
    }
    $chkdskExit = (Get-Content -LiteralPath $exitFile -Raw).Trim()
    if ((Get-Sha256 $image) -ne $digest) { throw "$($case.Name) changed while Windows graded its copy" }

    # A Slate-written copy Windows rejected is never mounted.
    if (-not $case.Slate -or $passed) {
        # Windows reads a separate wrapper. Never sync its mount-time changes
        # into the original image that Slate will inspect (#856).
        $readCopy = Join-Path $scratch "$($case.Name)-read.img"
        $readVhd = [IO.Path]::ChangeExtension($readCopy, '.vhd')
        $manifestPath = Join-Path $manifests "$($case.Name).json"
        $allocationPath = Join-Path $allocations "$($case.Name).json"
        if ((Test-Path -LiteralPath $manifestPath) -or (Test-Path -LiteralPath $allocationPath) -or
            (Test-Path -LiteralPath $readVhd)) {
            throw "Stale Windows read capture for $($case.Name); use a fresh OutDir"
        }
        Copy-Item -LiteralPath $image -Destination $readCopy -Force
        if ((Get-Sha256 $readCopy) -ne $digest) { throw "Read copy differs for $($case.Name)" }
        try {
            $capture = Invoke-FixturePhase "$($case.Name)-windows-read" -PhaseTimeoutSeconds 600 {
                $readDiag = Join-Path $out "windows-read-diag\$($case.Name)"
                New-Item -ItemType Directory -Force -Path $readDiag | Out-Null
                $state = Initialize-VhdFromImg -ImagePath $readCopy -Diag $readDiag
                $letter = Mount-VhdAndGetLetter -Vhd $state.Vhd
                # One mount, so the names and the clusters are the same state (#875).
                @{
                    Read = Get-SlateWindowsManifest -Root "${letter}:\" -Case $case.Name -ImageSha256 $digest
                    Allocation = Get-SlateWindowsAllocation -Root "${letter}:\" -Case $case.Name -ImageSha256 $digest
                }
            }
        } finally {
            # Bound and verify cleanup; a leaked mount is a failure, including
            # when enumeration or hashing failed. The helper never syncs back.
            Invoke-FixturePhase "$($case.Name)-read-cleanup" -PhaseTimeoutSeconds 180 {
                if (Test-Path -LiteralPath $readVhd) {
                    if (@(Get-DiskImage -ImagePath $readVhd -ErrorAction Stop | Where-Object Attached).Count) {
                        Dismount-ScratchVhd -Vhd $readVhd -TimeoutSeconds 120
                    }
                    if (@(Get-DiskImage -ImagePath $readVhd -ErrorAction Stop | Where-Object Attached).Count) {
                        throw "Windows read wrapper remains mounted: $readVhd"
                    }
                    Remove-Item -LiteralPath $readVhd -Force -ErrorAction Stop
                }
                Remove-Item -LiteralPath $readCopy -Force -ErrorAction Stop
            }
        }
        if ((Get-Sha256 $image) -ne $digest) { throw "$($case.Name) changed during Windows read capture" }
        [IO.File]::WriteAllText($allocationPath, ($capture.Allocation | ConvertTo-Json -Depth 10),
            [Text.UTF8Encoding]::new($false))
        [IO.File]::WriteAllText($manifestPath, ($capture.Read | ConvertTo-Json -Depth 10),
            [Text.UTF8Encoding]::new($false))
    }

    if ($case.Slate) {
        $verdict = if ($passed) { 'accepted' } else { 'slate-defect' }
        if (-not $passed) { $defects += $case.Name }
        Write-Host "slate-write $($case.Name) $digest chkdsk=$verdict exit=$chkdskExit"
        $slateRows += "$($case.Name)`t$($case.Name).img`t$digest`t$verdict`t$chkdskExit"
        continue
    }
    $verdict = if ($passed) { 'clean' } else { 'not-clean' }
    Write-Host "slate-oracle-case $($case.Name) $($case.Origin) $digest chkdsk=$verdict exit=$chkdskExit"
    $rows += "$($case.Name)`t$($case.Origin)`t$($case.Name).img`t$digest`t$verdict`t$chkdskExit"
}

# LF line ends: the tables are read by bash on Linux.
if ($SlateWrites) {
    if ($slateRows.Count - 1 -ne $written.Count) {
        throw "Graded $($slateRows.Count - 1) Slate-written images, but the writer table has $($written.Count) rows"
    }
    [System.IO.File]::WriteAllText((Join-Path $out 'slate-writes-graded.tsv'), (($slateRows -join "`n") + "`n"),
        (New-Object System.Text.UTF8Encoding($false)))
    # A rejected copy is the writer's defect, not a failure of this run.
    $summary = @('### Windows grades of Slate-written copies', '',
        "$($written.Count) writer rows, $($defects.Count) Slate writer defect(s); table in slate-writes-graded.tsv.")
    foreach ($name in $defects) {
        $summary += "- Slate writer defect: ``$name`` was rejected by read-only chkdsk; its image and chkdsk log are kept in the artifact."
    }
    $summary | ForEach-Object { Write-Host $_ }
    if ($env:GITHUB_STEP_SUMMARY) { Add-Content -LiteralPath $env:GITHUB_STEP_SUMMARY -Value $summary }
} else {
    [System.IO.File]::WriteAllText((Join-Path $out 'cases.tsv'), (($rows -join "`n") + "`n"),
        (New-Object System.Text.UTF8Encoding($false)))
}
Remove-Item -LiteralPath $scratch -Recurse -Force -ErrorAction SilentlyContinue
