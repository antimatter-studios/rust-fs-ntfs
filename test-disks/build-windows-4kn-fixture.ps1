# Builds ntfs-4kn-read.img (#1150): an NTFS volume Windows formatted on a disk
# presenting 4096-byte logical sectors, holding the file set of
# ntfs-cli-read.img, plus
#   * ntfs-4kn-read.sha256.tsv: each file's path, size and the SHA-256
#     Get-FileHash computes, in ntfs-cli-read.sha256.tsv's format
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
# Run from the repository root on an elevated Windows host, after
# build-windows-native-read-fixtures.ps1, whose ntfs-cli-read.sha256.tsv names
# the files to write. A failed run leaves none of the three outputs.

param([string]$OutDir = 'test-disks')
$ErrorActionPreference = 'Stop'
. "$PSScriptRoot/../scripts/windows-format-geometry.ps1"

$SectorBytes = 4096
$VolumeBytes = 256MB
$out = [IO.Path]::GetFullPath($OutDir)
$image = Join-Path $out 'ntfs-4kn-read.img'
$hashes = Join-Path $out 'ntfs-4kn-read.sha256.tsv'
$record = Join-Path $out 'ntfs-4kn-read.ntfsinfo.txt'
$cliRead = Join-Path $out 'ntfs-cli-read.sha256.tsv'

if (-not $IsWindows) { throw 'the 4Kn fixture requires an elevated Windows host' }
$principal = [Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'the 4Kn fixture requires Administrator'
}
if (-not (Test-Path -LiteralPath $cliRead)) {
    throw "${cliRead} is missing: run test-disks/build-windows-native-read-fixtures.ps1 first, it names the files to write"
}
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

$tempRoot = if ($env:RUNNER_TEMP) { $env:RUNNER_TEMP } else { [IO.Path]::GetTempPath() }
$scratch = Join-Path $tempRoot ('ntfs-4kn-read-' + [guid]::NewGuid())
New-Item -ItemType Directory -Force -Path $scratch | Out-Null
foreach ($path in $image, $hashes, $record) { Remove-Item -LiteralPath $path -Force -ErrorAction SilentlyContinue }
$vhdx = $null
$done = $false
try {
    $made = New-SectorGeometryDisk $scratch $SectorBytes
    $vhdx = $made.path
    $tool = $made.attempts[-1].tool
    Write-Host "ntfs-4kn-read: disk made with $tool"
    $disk = Mount-DiskImage -ImagePath $vhdx -PassThru | Get-Disk
    if ($disk.LogicalSectorSize -ne $SectorBytes) {
        throw "the scratch disk presents $($disk.LogicalSectorSize)-byte logical sectors, not $SectorBytes"
    }
    Initialize-Disk -Number $disk.Number -PartitionStyle GPT | Out-Null
    # The size of ntfs-cli-read.img, so the two volumes differ in their
    # sectors and not in their extent.
    $partition = New-Partition -DiskNumber $disk.Number -Size $VolumeBytes -AssignDriveLetter
    if (-not $partition.DriveLetter) { throw 'the 4Kn partition has no drive letter' }
    $letter = $partition.DriveLetter
    $offset = [long]$partition.Offset
    $span = [long]$partition.Size
    Format-Volume -DriveLetter $letter -FileSystem NTFS -AllocationUnitSize 4096 `
        -NewFileSystemLabel 'CLIREAD4K' -Force -Confirm:$false | Out-Null

    $root = "${letter}:\"
    $rng = [Security.Cryptography.RandomNumberGenerator]::Create()
    $lines = foreach ($file in $files) {
        $bytes = [byte[]]::new($file.Size)
        if ($file.Size -gt 0) { $rng.GetBytes($bytes) }
        $path = Join-Path $root ($file.Path.TrimStart('/') -replace '/', '\')
        $parent = Split-Path -Parent $path
        if (-not (Test-Path -LiteralPath $parent)) { New-Item -ItemType Directory -Path $parent -Force | Out-Null }
        [IO.File]::WriteAllBytes($path, $bytes)
        $hash = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLowerInvariant()
        "$($file.Path)`t$($file.Size)`t$hash"
    }
    [IO.File]::WriteAllText($hashes, (($lines -join "`n") + "`n"), [Text.UTF8Encoding]::new($false))

    # Windows' account of the mounted volume, and its verdict on it.
    $info = @(& fsutil fsinfo ntfsinfo "${letter}:")
    if ($LASTEXITCODE -ne 0) { throw "fsutil fsinfo ntfsinfo ${letter}: failed" }
    $reported = Get-NtfsInfoField $info 'Bytes Per Sector'
    if ($reported -ne $SectorBytes) { throw "fsutil reports $reported bytes per sector, not $SectorBytes" }
    & chkdsk.exe "${letter}:" | Out-Host
    if ($LASTEXITCODE -ne 0) { throw "chkdsk found the 4Kn volume ${letter}: unclean" }

    # Windows' own detach, then the partition's bytes from the disk attached
    # again read-only, so nothing writes to it while it is copied.
    Dismount-DiskImage -ImagePath $vhdx -ErrorAction Stop | Out-Null
    $disk = Mount-DiskImage -ImagePath $vhdx -Access ReadOnly -NoDriveLetter -PassThru | Get-Disk
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
    Dismount-DiskImage -ImagePath $vhdx -ErrorAction Stop | Out-Null
    if ((Get-Item -LiteralPath $image).Length -ne $span) { throw "${image} is not the partition's $span bytes" }

    $digest = (Get-FileHash -LiteralPath $image -Algorithm SHA256).Hash.ToLowerInvariant()
    $head = (& git rev-parse HEAD).Trim()
    if ($LASTEXITCODE -ne 0 -or $head -notmatch '^[0-9a-f]{40}$') { throw 'cannot bind the 4Kn record to HEAD' }
    Set-Content -LiteralPath $record -Value (@("image sha256 : $digest", "git HEAD : $head", "disk tool : $tool") + $info)
    $done = $true
    Write-Host "ntfs-4kn-read: Windows formatted $span bytes in $SectorBytes-byte sectors and chkdsk found it clean"
} finally {
    if ($vhdx -and (Test-Path -LiteralPath $vhdx)) {
        if ((Get-DiskImage -ImagePath $vhdx).Attached) { Dismount-DiskImage -ImagePath $vhdx -ErrorAction Stop | Out-Null }
        if ((Get-DiskImage -ImagePath $vhdx).Attached) { throw "the scratch VHDX remains attached: $vhdx" }
    }
    Remove-Item -LiteralPath $scratch -Recurse -Force -ErrorAction SilentlyContinue
    if (-not $done) {
        foreach ($path in $image, $hashes, $record) { Remove-Item -LiteralPath $path -Force -ErrorAction SilentlyContinue }
    }
}
