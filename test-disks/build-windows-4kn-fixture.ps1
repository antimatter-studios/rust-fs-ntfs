# Builds ntfs-4kn-read.img (#1150): an NTFS volume Windows formatted on a disk
# presenting 4096-byte logical sectors, holding the file set of
# ntfs-cli-read.img, plus
#   * ntfs-4kn-read.sha256.tsv: each file's path, size and the SHA-256
#     Get-FileHash computes, in ntfs-cli-read.sha256.tsv's format
#   * ntfs-4kn-read.index.tsv: the same for the 400 files of /index-blocks,
#     a directory large enough for index blocks, written beside that file set
#   * ntfs-4kn-read.ntfsinfo.txt: fsutil fsinfo ntfsinfo of the mounted
#     volume, after the image's SHA-256, the commit that built it and the
#     tool that made the disk
#
# New-WindowsFixture cannot make this one: it wraps a raw image in a VHD, and
# a VHD always presents 512-byte sectors. The disk is New-SectorGeometryDisk's
# VHDX instead. Windows' own detach flushes the volume and writes its final
# checkpoint; the VHDX is then attached again read-only and the partition's
# bytes are read through the disk device into the raw image, as a VHDX file
# does not hold the disk's bytes at their own offsets.
#
# ONE PHASE PER PROCESS (#1352). The build has stalled the hosted runner
# itself: the step's timeout-minutes, the process watchdog and every
# `if: always()` step after it never ran, and the job log and artifacts were
# lost with the job. The job's step list survived, and it records the step
# that was in progress. So each storage operation is one -Phase, run as its
# own process under scripts/run-fixture-process.py and as its own CI step,
# and a stall names its operation even when nothing else comes home. The
# phases share what they learn through $StateDir/state.json; each looks the
# scratch disk up again by its VHDX path rather than trusting a disk number.
#
# Run from the repository root on an elevated Windows host, after
# build-windows-native-read-fixtures.ps1, whose ntfs-cli-read.sha256.tsv names
# the files to write, with each phase in this order:
#   create attach initialize partition format populate fsutil chkdsk
#   detach attach-readonly export detach-readonly record
# and then cleanup, whatever the others did. cleanup detaches the scratch
# disk, and unless record finished, it leaves none of the three outputs.

param(
    [Parameter(Mandatory=$true)]
    [ValidateSet('create', 'attach', 'initialize', 'partition', 'format', 'populate', 'fsutil',
        'chkdsk', 'detach', 'attach-readonly', 'export', 'detach-readonly', 'record', 'cleanup')] [string]$Phase,
    [string]$OutDir = 'test-disks',
    [string]$StateDir = 'tmp/4kn-fixture',
    [ValidateRange(1, 120)] [int]$PhaseTimeoutSeconds = 60
)
$ErrorActionPreference = 'Stop'
. "$PSScriptRoot/../scripts/windows-format-geometry.ps1"
. "$PSScriptRoot/windows-fixture.ps1"

function Write-4KnFixturePhase {
    param([string]$LogPath, [string]$PhaseName, [string]$State)
    $line = '{0} phase={1} state={2}' -f (Get-Date).ToUniversalTime().ToString('o'), $PhaseName, $State
    [IO.File]::AppendAllText($LogPath, $line + "`n", [Text.UTF8Encoding]::new($false))
    Write-Host $line
}

function Invoke-4KnFixturePhase {
    param(
        [Parameter(Mandatory=$true)] [string]$PhaseName,
        [Parameter(Mandatory=$true)] [scriptblock]$PhaseAction,
        [Parameter(Mandatory=$true)] [string]$LogPath,
        [ValidateRange(1, 120)] [int]$PhaseTimeoutSeconds = 60,
        [switch]$CheckNativeExit
    )
    Write-4KnFixturePhase $LogPath $PhaseName 'started'
    try {
        $output = Invoke-FixturePhase -PhaseName $PhaseName -PhaseAction $PhaseAction `
            -PhaseTimeoutSeconds $PhaseTimeoutSeconds -CheckNativeExit:$CheckNativeExit
        Write-4KnFixturePhase $LogPath $PhaseName 'finished'
        $output
    } catch {
        Write-4KnFixturePhase $LogPath $PhaseName ('failed ' + ($_.Exception.Message -replace '[\r\n]+', ' '))
        throw
    }
}

function Save-4KnState([System.Collections.IDictionary]$State) {
    ConvertTo-Json -InputObject $State | Set-Content -LiteralPath $statePath -Encoding utf8
}

# The scratch disk as Windows sees it now, found by its VHDX path.
function Get-4KnScratchDisk([string]$PhaseName) {
    Invoke-4KnFixturePhase @phaseSettings -PhaseName $PhaseName -PhaseAction {
        Get-DiskImage -ImagePath $vhdx | Get-Disk
    }
}

$SectorBytes = 4096
$VolumeBytes = 256MB
$out = [IO.Path]::GetFullPath($OutDir)
$image = Join-Path $out 'ntfs-4kn-read.img'
$hashes = Join-Path $out 'ntfs-4kn-read.sha256.tsv'
$record = Join-Path $out 'ntfs-4kn-read.ntfsinfo.txt'
# A directory of 400 files, so the volume holds index blocks (#1384); kept
# apart from the file set of ntfs-cli-read.img that fixture_4kn_windows.rs
# holds the volume to.
$indexHashes = Join-Path $out 'ntfs-4kn-read.index.tsv'
$IndexDirectory = '/index-blocks'
$IndexFiles = 400
$cliRead = Join-Path $out 'ntfs-cli-read.sha256.tsv'
$lifecycle = Join-Path $out 'ntfs-4kn-read.lifecycle.log'
$stateRoot = [IO.Path]::GetFullPath($StateDir)
$statePath = Join-Path $stateRoot 'state.json'
$infoPath = Join-Path $stateRoot 'ntfsinfo.txt'
$phaseSettings = @{ LogPath = $lifecycle; PhaseTimeoutSeconds = $PhaseTimeoutSeconds }
if ($Phase -eq 'create') {
    [IO.File]::WriteAllText($lifecycle, '', [Text.UTF8Encoding]::new($false))
}
Write-4KnFixturePhase $lifecycle "process-$Phase" 'started'

# A failure outside any storage call is retained in the lifecycle too.
try {
    if (-not $IsWindows) { throw 'the 4Kn fixture requires an elevated Windows host' }
    $principal = [Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()
    if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
        throw 'the 4Kn fixture requires Administrator'
    }

    if ($Phase -eq 'create') {
        New-Item -ItemType Directory -Force -Path $stateRoot | Out-Null
        foreach ($path in $image, $hashes, $indexHashes, $record, $statePath, $infoPath) {
            Remove-Item -LiteralPath $path -Force -ErrorAction SilentlyContinue
        }
        $tempRoot = if ($env:RUNNER_TEMP) { $env:RUNNER_TEMP } else { [IO.Path]::GetTempPath() }
        $state = [ordered]@{ scratch = (Join-Path $tempRoot ('ntfs-4kn-read-' + [guid]::NewGuid()));
            vhdx = $null; tool = $null; letter = $null; offset = $null; span = $null; done = $false }
        # Saved before the disk exists, so cleanup can find the scratch directory.
        Save-4KnState $state
    } elseif (Test-Path -LiteralPath $statePath) {
        $state = Get-Content -Raw -LiteralPath $statePath | ConvertFrom-Json -AsHashtable
    } elseif ($Phase -eq 'cleanup') {
        $state = $null
    } else {
        throw "${statePath} is missing: phase create has not run"
    }
    if ($state) {
        $scratch = $state.scratch
        $vhdx = $state.vhdx
        $letter = $state.letter
        $offset = [long]$state.offset
        $span = [long]$state.span
    }

    switch ($Phase) {
        'create' {
            if (-not (Test-Path -LiteralPath $cliRead)) {
                throw "${cliRead} is missing: run test-disks/build-windows-native-read-fixtures.ps1 first, it names the files to write"
            }
            New-Item -ItemType Directory -Force -Path $scratch | Out-Null
            $made = Invoke-4KnFixturePhase @phaseSettings -PhaseName 'New-SectorGeometryDisk' -PhaseAction {
                New-SectorGeometryDisk $scratch $SectorBytes
            }
            $state.vhdx = $made.path
            $state.tool = $made.attempts[-1].tool
            Save-4KnState $state
            Write-Host "ntfs-4kn-read: disk made with $($state.tool)"
        }
        'attach' {
            $disk = Invoke-4KnFixturePhase @phaseSettings -PhaseName 'Mount-DiskImage' -PhaseAction {
                Mount-DiskImage -ImagePath $vhdx -PassThru | Get-Disk
            }
            if ($disk.LogicalSectorSize -ne $SectorBytes) {
                throw "the scratch disk presents $($disk.LogicalSectorSize)-byte logical sectors, not $SectorBytes"
            }
        }
        'initialize' {
            $disk = Get-4KnScratchDisk 'Get-Disk'
            Invoke-4KnFixturePhase @phaseSettings -PhaseName 'Initialize-Disk' -PhaseAction {
                Initialize-Disk -Number $disk.Number -PartitionStyle GPT | Out-Null
            }
        }
        'partition' {
            $disk = Get-4KnScratchDisk 'Get-Disk'
            # The size of ntfs-cli-read.img, so the two volumes differ in their
            # sectors and not in their extent.
            $partition = Invoke-4KnFixturePhase @phaseSettings -PhaseName 'New-Partition' -PhaseAction {
                New-Partition -DiskNumber $disk.Number -Size $VolumeBytes -AssignDriveLetter
            }
            if (-not $partition.DriveLetter) { throw 'the 4Kn partition has no drive letter' }
            $state.letter = [string]$partition.DriveLetter
            $state.offset = [long]$partition.Offset
            $state.span = [long]$partition.Size
            Save-4KnState $state
        }
        'format' {
            Invoke-4KnFixturePhase @phaseSettings -PhaseName 'Format-Volume' -PhaseAction {
                Format-Volume -DriveLetter $letter -FileSystem NTFS -AllocationUnitSize 4096 `
                    -NewFileSystemLabel 'CLIREAD4K' -Force -Confirm:$false | Out-Null
            }
        }
        'populate' {
            # The file set of ntfs-cli-read.img, as the path and size of each line.
            $files = @(foreach ($line in [IO.File]::ReadAllLines($cliRead, [Text.Encoding]::UTF8)) {
                if ($line -eq '') { continue }
                $columns = $line -split "`t"
                if ($columns.Count -ne 3 -or -not $columns[0].StartsWith('/')) {
                    throw "${cliRead}: '$line' is not path, size and sha256"
                }
                [pscustomobject]@{ Path = $columns[0]; Size = [long]$columns[1] }
            })
            if ($files.Count -eq 0) { throw "${cliRead} names no file" }
            $root = "${letter}:\"
            $rng = [Security.Cryptography.RandomNumberGenerator]::Create()
            $lines = Invoke-4KnFixturePhase @phaseSettings -PhaseName 'populate-files' -PhaseAction {
                foreach ($file in $files) {
                    $bytes = [byte[]]::new($file.Size)
                    if ($file.Size -gt 0) { $rng.GetBytes($bytes) }
                    $path = Join-Path $root ($file.Path.TrimStart('/') -replace '/', '\')
                    $parent = Split-Path -Parent $path
                    if (-not (Test-Path -LiteralPath $parent)) { New-Item -ItemType Directory -Path $parent -Force | Out-Null }
                    [IO.File]::WriteAllBytes($path, $bytes)
                    $hash = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLowerInvariant()
                    "$($file.Path)`t$($file.Size)`t$hash"
                }
            }
            [IO.File]::WriteAllText($hashes, (($lines -join "`n") + "`n"), [Text.UTF8Encoding]::new($false))
            $indexLines = Invoke-4KnFixturePhase @phaseSettings -PhaseName 'populate-index-blocks' -PhaseAction {
                $directory = Join-Path $root ($IndexDirectory.TrimStart('/'))
                New-Item -ItemType Directory -Path $directory -Force | Out-Null
                for ($i = 0; $i -lt $IndexFiles; $i++) {
                    $name = 'f{0:D3}.txt' -f $i
                    $bytes = [byte[]]::new((($i * 53) % 900) + 1)
                    $rng.GetBytes($bytes)
                    $path = Join-Path $directory $name
                    [IO.File]::WriteAllBytes($path, $bytes)
                    $hash = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLowerInvariant()
                    "$IndexDirectory/$name`t$($bytes.Length)`t$hash"
                }
            }
            [IO.File]::WriteAllText($indexHashes, (($indexLines -join "`n") + "`n"), [Text.UTF8Encoding]::new($false))
        }
        'fsutil' {
            # Windows' account of the mounted volume.
            $info = @(Invoke-4KnFixturePhase @phaseSettings -PhaseName 'fsutil' -CheckNativeExit -PhaseAction {
                & fsutil fsinfo ntfsinfo "${letter}:"
            })
            if ($LASTEXITCODE -ne 0) { throw "fsutil fsinfo ntfsinfo ${letter}: failed" }
            $reported = Get-NtfsInfoField $info 'Bytes Per Sector'
            if ($reported -ne $SectorBytes) { throw "fsutil reports $reported bytes per sector, not $SectorBytes" }
            Set-Content -LiteralPath $infoPath -Value $info
        }
        'chkdsk' {
            # Windows' verdict on the mounted volume.
            Invoke-4KnFixturePhase @phaseSettings -PhaseName 'chkdsk.exe' -CheckNativeExit -PhaseAction {
                & chkdsk.exe "${letter}:" | Out-Host
            }
            if ($LASTEXITCODE -ne 0) { throw "chkdsk found the 4Kn volume ${letter}: unclean" }
        }
        'detach' {
            # Windows' own detach, then the partition's bytes from the disk
            # attached again read-only, so nothing writes to it while it is copied.
            Invoke-4KnFixturePhase @phaseSettings -PhaseName 'Dismount-DiskImage' -PhaseAction {
                Dismount-DiskImage -ImagePath $vhdx -ErrorAction Stop | Out-Null
            }
        }
        'attach-readonly' {
            $null = Invoke-4KnFixturePhase @phaseSettings -PhaseName 'Mount-DiskImage-readonly' -PhaseAction {
                Mount-DiskImage -ImagePath $vhdx -Access ReadOnly -NoDriveLetter -PassThru | Get-Disk
            }
        }
        'export' {
            $disk = Get-4KnScratchDisk 'Get-Disk-readonly'
            Invoke-4KnFixturePhase @phaseSettings -PhaseName 'export-PhysicalDrive' -PhaseAction {
                $device = [IO.FileStream]::new("\\.\PhysicalDrive$($disk.Number)", [IO.FileMode]::Open,
                    [IO.FileAccess]::Read, [IO.FileShare]::ReadWrite, 1)
                try {
                    $sink = [IO.File]::Create($image)
                    try {
                        $null = $device.Seek($offset, [IO.SeekOrigin]::Begin)
                        $buffer = [byte[]]::new(16MB)
                        $remaining = $span
                        while ($remaining -gt 0) {
                            $count = [int][Math]::Min([long]$buffer.Length, $remaining)
                            $device.ReadExactly($buffer, 0, $count)
                            $sink.Write($buffer, 0, $count)
                            $remaining -= $count
                        }
                        $sink.Flush($true)
                    } finally { $sink.Dispose() }
                } finally { $device.Dispose() }
            }
            if ((Get-Item -LiteralPath $image).Length -ne $span) { throw "${image} is not the partition's $span bytes" }
        }
        'detach-readonly' {
            Invoke-4KnFixturePhase @phaseSettings -PhaseName 'Dismount-DiskImage-readonly' -PhaseAction {
                Dismount-DiskImage -ImagePath $vhdx -ErrorAction Stop | Out-Null
            }
        }
        'record' {
            $info = @(Get-Content -LiteralPath $infoPath)
            $digest = Invoke-4KnFixturePhase @phaseSettings -PhaseName 'Get-FileHash-image' -PhaseAction {
                (Get-FileHash -LiteralPath $image -Algorithm SHA256).Hash.ToLowerInvariant()
            }
            $head = Invoke-4KnFixturePhase @phaseSettings -PhaseName 'git-rev-parse' -CheckNativeExit -PhaseAction {
                (& git rev-parse HEAD).Trim()
            }
            if ($LASTEXITCODE -ne 0 -or $head -notmatch '^[0-9a-f]{40}$') { throw 'cannot bind the 4Kn record to HEAD' }
            Set-Content -LiteralPath $record -Value (@("image sha256 : $digest", "git HEAD : $head", "disk tool : $($state.tool)") + $info)
            $state.done = $true
            Save-4KnState $state
            Write-Host "ntfs-4kn-read: Windows formatted $span bytes in $SectorBytes-byte sectors and chkdsk found it clean"
        }
        'cleanup' {
            try {
                if ($vhdx -and (Test-Path -LiteralPath $vhdx)) {
                    Invoke-4KnFixturePhase @phaseSettings -PhaseName 'cleanup-DiskImage' -PhaseAction {
                        if ((Get-DiskImage -ImagePath $vhdx).Attached) { Dismount-DiskImage -ImagePath $vhdx -ErrorAction Stop | Out-Null }
                        if ((Get-DiskImage -ImagePath $vhdx).Attached) { throw "the scratch VHDX remains attached: $vhdx" }
                    }
                }
            } finally {
                if ($scratch) { Remove-Item -LiteralPath $scratch -Recurse -Force -ErrorAction SilentlyContinue }
                if (-not ($state -and $state.done)) {
                    foreach ($path in $image, $hashes, $indexHashes, $record) { Remove-Item -LiteralPath $path -Force -ErrorAction SilentlyContinue }
                }
            }
        }
    }
} catch {
    Write-4KnFixturePhase $lifecycle "process-$Phase" ('failed ' + ($_.Exception.Message -replace '[\r\n]+', ' '))
    throw
}
Write-4KnFixturePhase $lifecycle "process-$Phase" 'finished'
