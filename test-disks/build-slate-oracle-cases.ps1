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
# A case that cannot be graded at all -- no image, no chkdsk record -- stops
# the run. A case Windows grades and does not accept is recorded as
# `not-clean` and left for the comparison to report.
#
# Run from the repository root on an elevated Windows host with rust-img-vhd
# on PATH; CI installs the pinned rust-img-vhd before invoking this.
param(
    [Parameter(Mandatory=$true)] [string]$Cli,
    [string]$OutDir = 'tmp/slate-oracle'
)

$ErrorActionPreference = 'Stop'

. "$PSScriptRoot\..\scripts\fs-windows-test-harness\_lib.ps1"
. "$PSScriptRoot\windows-fixture.ps1"

$cli = (Resolve-Path -LiteralPath $Cli).Path
$winChkdsk = (Resolve-Path "$PSScriptRoot\..\scripts\fs-windows-test-harness\win-chkdsk.ps1").Path
$out = [System.IO.Path]::GetFullPath($OutDir)
$images = Join-Path $out 'images'
$scratch = Join-Path $out 'scratch'
New-Item -ItemType Directory -Force -Path $images, $scratch | Out-Null

function Get-Sha256([string]$Path) {
    (Get-FileHash -Algorithm SHA256 -LiteralPath $Path).Hash.ToLowerInvariant()
}

$cases = @(
    @{ Name = 'win-4k';   Origin = 'windows'; Cluster = 4096 },
    @{ Name = 'win-64k';  Origin = 'windows'; Cluster = 65536 },
    @{ Name = 'ours-4k';  Origin = 'ours';    Cluster = 4096 },
    @{ Name = 'ours-64k'; Origin = 'ours';    Cluster = 65536 }
)

foreach ($case in $cases) {
    $image = Join-Path $images "$($case.Name).img"
    $label = ($case.Name -replace '-', '').ToUpperInvariant()
    if ($case.Origin -eq 'windows') {
        New-WindowsFixture -ImagePath $image -Label $label `
            -AllocationUnitSize $case.Cluster -Populate { param($root) }
    } else {
        Remove-Item -LiteralPath $image -Force -ErrorAction SilentlyContinue
        & $cli mkfs --size 256M --label $label --cluster-size $case.Cluster $image |
            Set-Content -LiteralPath (Join-Path $out "$($case.Name).mkfs.json")
        if ($LASTEXITCODE -ne 0) { throw "mkfs.ntfs exited $LASTEXITCODE for $($case.Name)" }
    }
    if (-not (Test-Path -LiteralPath $image)) { throw "$($case.Name) left no image at $image" }
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

    $verdict = if ($passed) { 'clean' } else { 'not-clean' }
    Write-Host "slate-oracle-case $($case.Name) $($case.Origin) $digest chkdsk=$verdict exit=$chkdskExit"
    $rows += "$($case.Name)`t$($case.Origin)`t$($case.Name).img`t$digest`t$verdict`t$chkdskExit"
}

# LF line ends: the table is read by bash on Linux.
[System.IO.File]::WriteAllText((Join-Path $out 'cases.tsv'), (($rows -join "`n") + "`n"),
    (New-Object System.Text.UTF8Encoding($false)))
Remove-Item -LiteralPath $scratch -Recurse -Force -ErrorAction SilentlyContinue
