# Has Windows grade what this crate wrote to the 4Kn volume (#1396).
#
# tests/write_4kn_windows.rs changes a copy of ntfs-4kn-read.img and writes
# ntfs-4kn-write.expected.tsv: every directory (`dir`, path) and file (`file`,
# path, size, SHA-256) the volume must then hold. This script puts that image
# on a disk presenting 4096-byte logical sectors, attaches the disk again
# read-only, runs chkdsk, and compares Windows' own listing and Get-FileHash
# with the report. A file or directory missing, extra, or of other bytes
# fails, as does an unclean chkdsk.
#
# READ-ONLY, so Windows judges the bytes this crate wrote. Mounted writable,
# Windows rewrites its own transaction log ($Extend\$RmMetadata\$TxfLog)
# while chkdsk reads it, and chkdsk then reports that directory's index
# (runs 45727929786 and 45738310732) although its record and its files' were
# byte for byte what Windows had formatted. -Control grades ntfs-4kn-read.img,
# Windows' own volume, the same way: a method that refuses that one cannot
# judge anything.
#
# One phase per process, for the reason build-windows-4kn-fixture.ps1 gives
# (#1352): each storage operation is its own CI step under
# scripts/run-fixture-process.py, so a stalled runner names the operation.
#
# Run from the repository root on an elevated Windows host, each phase in
# this order:
#   create attach partition import remount chkdsk enumerate
# and then cleanup, whatever the others did.

param(
    [Parameter(Mandatory=$true)]
    [ValidateSet('create', 'attach', 'partition', 'import', 'remount', 'chkdsk', 'enumerate', 'cleanup')]
    [string]$Phase,
    [string]$OutDir = 'test-disks',
    [string]$StateDir = 'tmp/4kn-write',
    [ValidateRange(1, 120)] [int]$PhaseTimeoutSeconds = 60,
    [switch]$Control
)
$ErrorActionPreference = 'Stop'
. "$PSScriptRoot/../scripts/windows-format-geometry.ps1"
. "$PSScriptRoot/windows-fixture.ps1"

$SectorBytes = 4096
$out = [IO.Path]::GetFullPath($OutDir)
$name = if ($Control) { 'ntfs-4kn-control' } else { 'ntfs-4kn-write' }
$image = Join-Path $out $(if ($Control) { 'ntfs-4kn-read.img' } else { 'ntfs-4kn-write.img' })
$expected = Join-Path $out 'ntfs-4kn-write.expected.tsv'
$lifecycle = Join-Path $out "$name.lifecycle.log"
if ($Control) {
    if ($Phase -eq 'enumerate') { throw 'the control has no report to enumerate against' }
    $StateDir = "$StateDir-control"
}
$stateRoot = [IO.Path]::GetFullPath($StateDir)
$statePath = Join-Path $stateRoot 'state.json'

function Write-4KnGradeLine([string]$PhaseName, [string]$State) {
    $line = '{0} phase={1} state={2}' -f (Get-Date).ToUniversalTime().ToString('o'), $PhaseName, $State
    [IO.File]::AppendAllText($lifecycle, $line + "`n", [Text.UTF8Encoding]::new($false))
    Write-Host $line
}

function Invoke-4KnGradeCall {
    param([Parameter(Mandatory=$true)] [string]$Name, [Parameter(Mandatory=$true)] [scriptblock]$Action,
        [switch]$CheckNativeExit)
    Write-4KnGradeLine $Name 'started'
    try {
        $output = Invoke-FixturePhase -PhaseName $Name -PhaseAction $Action `
            -PhaseTimeoutSeconds $PhaseTimeoutSeconds -CheckNativeExit:$CheckNativeExit
        Write-4KnGradeLine $Name 'finished'
        $output
    } catch {
        Write-4KnGradeLine $Name ('failed ' + ($_.Exception.Message -replace '[\r\n]+', ' '))
        throw
    }
}

if (-not $IsWindows) { throw 'grading the 4Kn volume requires an elevated Windows host' }
$principal = [Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'grading the 4Kn volume requires Administrator'
}

if ($Phase -eq 'create') {
    [IO.File]::WriteAllText($lifecycle, '', [Text.UTF8Encoding]::new($false))
    $needed = if ($Control) { @($image) } else { @($image, $expected) }
    foreach ($path in $needed) {
        if (-not (Test-Path -LiteralPath $path)) {
            throw "${path} is missing: run cargo test --test write_4kn_windows first"
        }
    }
    New-Item -ItemType Directory -Force -Path $stateRoot | Out-Null
    $tempRoot = if ($env:RUNNER_TEMP) { $env:RUNNER_TEMP } else { [IO.Path]::GetTempPath() }
    $state = [ordered]@{ scratch = (Join-Path $tempRoot ('ntfs-4kn-write-' + [guid]::NewGuid()));
        vhdx = $null; letter = $null; offset = $null }
    # Saved before the disk exists, so cleanup can find the scratch directory.
    ConvertTo-Json -InputObject $state | Set-Content -LiteralPath $statePath -Encoding utf8
} elseif (Test-Path -LiteralPath $statePath) {
    $state = Get-Content -Raw -LiteralPath $statePath | ConvertFrom-Json -AsHashtable
} elseif ($Phase -eq 'cleanup') {
    return
} else {
    throw "${statePath} is missing: phase create has not run"
}
$vhdx = $state.vhdx

function Save-4KnGradeState {
    ConvertTo-Json -InputObject $state | Set-Content -LiteralPath $statePath -Encoding utf8
}

function Get-4KnGradeDisk {
    Invoke-4KnGradeCall 'Get-Disk' { Get-DiskImage -ImagePath $vhdx | Get-Disk }
}

switch ($Phase) {
    'create' {
        New-Item -ItemType Directory -Force -Path $state.scratch | Out-Null
        $scratch = $state.scratch
        $made = Invoke-4KnGradeCall 'New-SectorGeometryDisk' { New-SectorGeometryDisk $scratch $SectorBytes }
        $state.vhdx = $made.path
        Save-4KnGradeState
    }
    'attach' {
        $disk = Invoke-4KnGradeCall 'Mount-DiskImage' { Mount-DiskImage -ImagePath $vhdx -PassThru | Get-Disk }
        if ($disk.LogicalSectorSize -ne $SectorBytes) {
            throw "the scratch disk presents $($disk.LogicalSectorSize)-byte logical sectors, not $SectorBytes"
        }
        Invoke-4KnGradeCall 'Initialize-Disk' { Initialize-Disk -Number $disk.Number -PartitionStyle GPT | Out-Null }
    }
    'partition' {
        $disk = Get-4KnGradeDisk
        $length = (Get-Item -LiteralPath $image).Length
        # No drive letter: nothing may mount the partition before its bytes are there.
        $partition = Invoke-4KnGradeCall 'New-Partition' {
            New-Partition -DiskNumber $disk.Number -Size $length
        }
        if ($partition.Size -ne $length) {
            throw "the partition is $($partition.Size) bytes, the image $length"
        }
        $state.offset = [long]$partition.Offset
        Save-4KnGradeState
    }
    'import' {
        $disk = Get-4KnGradeDisk
        $offset = [long]$state.offset
        # Offline, so the volume layer cannot lock the partition against the write.
        Invoke-4KnGradeCall 'Set-Disk-offline' { Set-Disk -Number $disk.Number -IsOffline $true }
        try {
            Invoke-4KnGradeCall 'import-PhysicalDrive' {
                $source = [IO.File]::OpenRead($image)
                try {
                    $device = [IO.FileStream]::new("\\.\PhysicalDrive$($disk.Number)", [IO.FileMode]::Open,
                        [IO.FileAccess]::ReadWrite, [IO.FileShare]::ReadWrite, 1)
                    try {
                        $null = $device.Seek($offset, [IO.SeekOrigin]::Begin)
                        $buffer = [byte[]]::new(16MB)
                        $remaining = $source.Length
                        while ($remaining -gt 0) {
                            $count = [int][Math]::Min([long]$buffer.Length, $remaining)
                            $source.ReadExactly($buffer, 0, $count)
                            $device.Write($buffer, 0, $count)
                            $remaining -= $count
                        }
                        $device.Flush($true)
                    } finally { $device.Dispose() }
                } finally { $source.Dispose() }
            }
        } finally {
            Invoke-4KnGradeCall 'Set-Disk-online' { Set-Disk -Number $disk.Number -IsOffline $false }
        }
        Invoke-4KnGradeCall 'Dismount-DiskImage' { Dismount-DiskImage -ImagePath $vhdx -ErrorAction Stop | Out-Null }
    }
    'remount' {
        # Attached again, so ntfs.sys mounts the imported volume afresh, and
        # read-only, so nothing changes it before or while it is judged.
        $disk = Invoke-4KnGradeCall 'Mount-DiskImage-readonly' {
            Mount-DiskImage -ImagePath $vhdx -Access ReadOnly -PassThru | Get-Disk
        }
        $offset = [long]$state.offset
        $letter = Invoke-4KnGradeCall 'drive-letter' {
            $partition = Get-Partition -DiskNumber $disk.Number | Where-Object Offset -eq $offset
            if (-not $partition) { throw "no partition at offset $offset" }
            if (-not $partition.DriveLetter) {
                $partition | Add-PartitionAccessPath -AssignDriveLetter
                $partition = Get-Partition -DiskNumber $disk.Number | Where-Object Offset -eq $offset
            }
            [string]$partition.DriveLetter
        }
        if (-not ([string]$letter).Trim([char]0)) { throw 'the imported partition has no drive letter' }
        $state.letter = [string]$letter
        Save-4KnGradeState
    }
    'chkdsk' {
        $letter = $state.letter
        Invoke-4KnGradeCall 'chkdsk.exe' -CheckNativeExit { & chkdsk.exe "${letter}:" | Out-Host }
    }
    'enumerate' {
        $letter = $state.letter
        $seen = @(Invoke-4KnGradeCall 'enumerate-and-hash' {
            $root = "${letter}:\"
            # Windows adds these two when it mounts a volume.
            $own = @('System Volume Information', '$RECYCLE.BIN')
            $items = foreach ($top in Get-ChildItem -LiteralPath $root -Force) {
                if ($own -contains $top.Name) { continue }
                $top
                if ($top.PSIsContainer) {
                    Get-ChildItem -LiteralPath $top.FullName -Recurse -Force -ErrorAction Stop
                }
            }
            foreach ($item in $items) {
                $path = '/' + ($item.FullName.Substring($root.Length) -replace '\\', '/')
                if ($item.PSIsContainer) {
                    "dir`t$path"
                } else {
                    $hash = (Get-FileHash -LiteralPath $item.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
                    "file`t$path`t$($item.Length)`t$hash"
                }
            }
        })
        $listing = Join-Path $stateRoot 'windows-listing.tsv'
        [IO.File]::WriteAllText($listing, (($seen -join "`n") + "`n"), [Text.UTF8Encoding]::new($false))
        $wanted = @([IO.File]::ReadAllLines($expected, [Text.Encoding]::UTF8) | Where-Object { $_ -ne '' })
        $differences = @(Compare-Object -ReferenceObject $wanted -DifferenceObject $seen -CaseSensitive)
        if ($differences.Count -gt 0) {
            foreach ($difference in $differences | Select-Object -First 40) {
                $side = if ($difference.SideIndicator -eq '<=') { 'expected only' } else { 'Windows only' }
                Write-Host "${side}: $($difference.InputObject)"
            }
            throw "Windows' listing differs from the report in $($differences.Count) lines"
        }
        Write-Host "ntfs-4kn-write: Windows lists and hashes all $($wanted.Count) entries as written"
    }
    'cleanup' {
        try {
            if ($vhdx -and (Test-Path -LiteralPath $vhdx)) {
                Invoke-4KnGradeCall 'cleanup-DiskImage' {
                    if ((Get-DiskImage -ImagePath $vhdx).Attached) { Dismount-DiskImage -ImagePath $vhdx -ErrorAction Stop | Out-Null }
                    if ((Get-DiskImage -ImagePath $vhdx).Attached) { throw "the scratch VHDX remains attached: $vhdx" }
                }
            }
        } finally {
            Remove-Item -LiteralPath $state.scratch -Recurse -Force -ErrorAction SilentlyContinue
        }
    }
}
