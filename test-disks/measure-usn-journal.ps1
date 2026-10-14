# Measures what Windows does with a volume's change journal ($UsnJrnl) in the
# states a writer that cannot append to it could leave the volume in (#479).
#
# Each experiment starts from a copy of test-disks/ntfs-usnjrnl.img, a volume
# whose journal Windows created and wrote to, built by
# build-windows-native-read-fixtures.ps1, and records what `fsutil usn
# queryjournal` answers once Windows has mounted the copy:
#
#   * control: the copy, mounted as it is;
#   * deleted: `fsutil usn deletejournal /n` run on it, so the copy holds what
#     Windows leaves on a volume whose journal it deleted;
#   * flag-0010: $VOLUME_INFORMATION's 0x0010 flag set before the mount
#     (VOLUME_DELETE_USN_UNDERWAY in the published layouts);
#   * flag-0008: its 0x0008 flag set before the mount (VOLUME_MOUNTED_ON_NT4).
#
# The $Volume flags are read before and after each mount. Every copy is kept
# as test-disks/usn-measure-<name>.img and the report is
# test-disks/usn-journal-measurements.txt.
#
# Run from the repository root on an elevated Windows host, after
# build-windows-native-read-fixtures.ps1.

$ErrorActionPreference = 'Stop'

. "$PSScriptRoot\..\scripts\fs-windows-test-harness\_lib.ps1"

$base = [System.IO.Path]::GetFullPath('test-disks/ntfs-usnjrnl.img')
$report = [System.IO.Path]::GetFullPath('test-disks/usn-journal-measurements.txt')
$tempRoot = if ($env:RUNNER_TEMP) { $env:RUNNER_TEMP } else { [System.IO.Path]::GetTempPath() }
if (-not (Test-Path $base)) {
    throw "missing $base; run test-disks/build-windows-native-read-fixtures.ps1 first"
}
Set-Content -Path $report -Value "# What Windows does with a volume's change journal (#479)" -Encoding UTF8

function Write-Report([string]$Line) {
    Add-Content -Path $report -Value $Line -Encoding UTF8
    Write-Host $Line
}

# The fixed fields of the boot sector this needs: where $MFT and $MFTMirr
# start and how long a file record is.
function Get-NtfsGeometry([System.IO.FileStream]$Stream) {
    $boot = New-Object byte[] 512
    $Stream.Seek(0, [System.IO.SeekOrigin]::Begin) | Out-Null
    if ($Stream.Read($boot, 0, 512) -ne 512) { throw 'short read of the boot sector' }
    $bps = [int64][BitConverter]::ToUInt16($boot, 0x0B)
    $spc = [int]$boot[0x0D]
    if ($spc -gt 0x80) { $spc = [int][Math]::Pow(2, 256 - $spc) }
    $cluster = $bps * $spc
    $cpr = [int]$boot[0x40]
    if ($cpr -gt 127) { $cpr -= 256 }
    $recordSize = if ($cpr -gt 0) { [int64]$cpr * $cluster } else { [int64]1 -shl (-$cpr) }
    @{
        RecordSize = $recordSize
        Mft        = [BitConverter]::ToInt64($boot, 0x30) * $cluster
        Mirror     = [BitConverter]::ToInt64($boot, 0x38) * $cluster
    }
}

# A file record with its update-sequence fixups undone.
function Read-FileRecord([System.IO.FileStream]$Stream, [int64]$Offset, [int64]$Size) {
    $record = New-Object byte[] $Size
    $Stream.Seek($Offset, [System.IO.SeekOrigin]::Begin) | Out-Null
    if ($Stream.Read($record, 0, $Size) -ne $Size) { throw "short read of the record at $Offset" }
    $usa = [BitConverter]::ToUInt16($record, 4)
    $count = [BitConverter]::ToUInt16($record, 6)
    for ($i = 1; $i -lt $count; $i++) {
        $tail = $i * 512 - 2
        if ($record[$tail] -ne $record[$usa] -or $record[$tail + 1] -ne $record[$usa + 1]) {
            throw "the record at $Offset is torn at sector $i"
        }
        $record[$tail] = $record[$usa + 2 * $i]
        $record[$tail + 1] = $record[$usa + 2 * $i + 1]
    }
    , $record
}

# Write a record back, redoing its fixups with the update sequence number it has.
function Write-FileRecord([System.IO.FileStream]$Stream, [int64]$Offset, [byte[]]$Record) {
    $out = [byte[]]$Record.Clone()
    $usa = [BitConverter]::ToUInt16($out, 4)
    $count = [BitConverter]::ToUInt16($out, 6)
    for ($i = 1; $i -lt $count; $i++) {
        $tail = $i * 512 - 2
        $out[$usa + 2 * $i] = $out[$tail]
        $out[$usa + 2 * $i + 1] = $out[$tail + 1]
        $out[$tail] = $out[$usa]
        $out[$tail + 1] = $out[$usa + 1]
    }
    $Stream.Seek($Offset, [System.IO.SeekOrigin]::Begin) | Out-Null
    $Stream.Write($out, 0, $out.Length)
}

# Offset in $Volume's record of $VOLUME_INFORMATION's 16-bit flags.
function Find-VolumeFlags([byte[]]$Record) {
    $at = [int][BitConverter]::ToUInt16($Record, 0x14)
    while ($at + 8 -le $Record.Length) {
        $type = [BitConverter]::ToUInt32($Record, $at)
        if ($type -eq [uint32]::MaxValue) { break }
        $length = [int][BitConverter]::ToUInt32($Record, $at + 4)
        if ($length -le 0) { break }
        if ($type -eq 0x70) { return $at + [int][BitConverter]::ToUInt16($Record, $at + 0x14) + 10 }
        $at += $length
    }
    throw '$Volume has no $VOLUME_INFORMATION'
}

function Get-VolumeFlags([string]$Image) {
    $stream = [System.IO.File]::Open($Image, 'Open', 'Read', 'ReadWrite')
    try {
        $g = Get-NtfsGeometry $stream
        $record = Read-FileRecord $stream ($g.Mft + 3 * $g.RecordSize) $g.RecordSize
        [BitConverter]::ToUInt16($record, (Find-VolumeFlags $record))
    } finally { $stream.Close() }
}

# Set `Bit` in $VOLUME_INFORMATION's flags, in $MFT and in $MFTMirr.
function Set-VolumeFlag([string]$Image, [uint16]$Bit) {
    $stream = [System.IO.File]::Open($Image, 'Open', 'ReadWrite', 'None')
    try {
        $g = Get-NtfsGeometry $stream
        foreach ($start in @($g.Mft, $g.Mirror)) {
            $offset = $start + 3 * $g.RecordSize
            $record = Read-FileRecord $stream $offset $g.RecordSize
            $at = Find-VolumeFlags $record
            $flags = [BitConverter]::ToUInt16($record, $at) -bor $Bit
            [BitConverter]::GetBytes([uint16]$flags).CopyTo($record, $at)
            Write-FileRecord $stream $offset $record
        }
        $stream.Flush($true)
    } finally { $stream.Close() }
}

# fsutil's answer, its error included: "not active" is an answer here.
function Get-JournalAnswer([string]$Volume) {
    $saved = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try {
        (& fsutil.exe usn queryjournal $Volume 2>&1 | Out-String).Trim()
    } finally {
        $ErrorActionPreference = $saved
    }
}

# Copy the journaled fixture to usn-measure-<Name>.img, let `Prepare` change
# the copy, mount it, let `OnMounted` act on the mounted volume, and report
# what `fsutil usn queryjournal` answers and the flags before and after.
function Measure-Journal {
    param(
        [Parameter(Mandatory=$true)] [string]$Name,
        [scriptblock]$Prepare = {},
        [scriptblock]$OnMounted = {}
    )
    $image = [System.IO.Path]::GetFullPath("test-disks/usn-measure-$Name.img")
    Write-Report ''
    Write-Report "## $Name"
    try {
        Copy-Item -LiteralPath $base -Destination $image -Force
        & $Prepare $image
        Write-Report ('flags before the mount: 0x{0:X4}' -f (Get-VolumeFlags $image))
        $diag = Join-Path $tempRoot "usn-measure-$Name"
        New-Item -ItemType Directory -Path $diag -Force | Out-Null
        $state = Initialize-VhdFromImg -ImagePath $image -Diag $diag
        try {
            $letter = Mount-VhdAndGetLetter -Vhd $state.Vhd
            $volume = "${letter}:"
            Write-Report "queryjournal once mounted:"
            Write-Report (Get-JournalAnswer $volume)
            & $OnMounted $volume
            # A deletion Windows has under way answers "being deleted" until
            # it completes; wait for a settled answer.
            $answer = Get-JournalAnswer $volume
            for ($i = 0; $i -lt 30 -and $answer -match 'being deleted'; $i++) {
                Start-Sleep -Seconds 2
                $answer = Get-JournalAnswer $volume
            }
            Write-Report "queryjournal settled:"
            Write-Report $answer
            Write-Report ('fsutil file layout of $Extend\$UsnJrnl:')
            Write-Report ((& fsutil.exe file layout "$volume\`$Extend\`$UsnJrnl" 2>&1 | Out-String).Trim())
            Sync-VhdToImg -Vhd $state.Vhd -ImagePath $image
        } finally {
            Get-DiskImage -ImagePath $state.Vhd -ErrorAction SilentlyContinue |
                Where-Object Attached |
                Dismount-DiskImage -ErrorAction SilentlyContinue | Out-Null
            Remove-Item $state.Vhd -Force -ErrorAction SilentlyContinue
        }
        Write-Report ('flags after Windows mounted it: 0x{0:X4}' -f (Get-VolumeFlags $image))
    } catch {
        Write-Report "experiment failed: $($_.Exception.Message)"
    }
}

Measure-Journal -Name 'control'
Measure-Journal -Name 'deleted' -OnMounted {
    param($volume)
    Write-Report 'fsutil usn deletejournal /n:'
    Write-Report ((& fsutil.exe usn deletejournal /n $volume 2>&1 | Out-String).Trim())
}
Measure-Journal -Name 'flag-0010' -Prepare { param($image) Set-VolumeFlag $image 0x0010 }
Measure-Journal -Name 'flag-0008' -Prepare { param($image) Set-VolumeFlag $image 0x0008 }
