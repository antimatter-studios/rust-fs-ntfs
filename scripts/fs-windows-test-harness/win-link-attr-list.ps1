# scripts/fs-windows-test-harness/win-link-attr-list.ps1 -- which attributes
# Windows moves out of a file's base record when hard links outgrow it
# (#1069, part of #1067).
#
# Creates the file at -Path (64 bytes) and the directory -LinkDir, then
# hard-links the file as <LinkDir>/many-0.txt, many-1.txt, ... -Links times,
# the names the cli-link recipe gives /many.txt. After every link it reads
# the file's base record with FSCTL_GET_NTFS_FILE_RECORD and notes what the
# record holds; after the last it reads the $ATTRIBUTE_LIST, resident or
# not, and every extension record the list names.
#
# attr-list.txt in -Diag records the measurement:
#   record=<n>                    the base record's number
#   link=<k> ... attr_list=<none|resident|nonresident> base_types=<...>
#                                 the base record after each link
#   base type=<t> ...             each attribute left in the base record
#   list type=<t> record=<n> ...  each $ATTRIBUTE_LIST entry
#   ext record=<n> type=<t> ...   each attribute in an extension record
#   moved=<types>                 the types the list places outside the base
#
# Args:
#   -ImagePath  Path on the VM to the .img file.
#   -Path       File path inside the volume, e.g. "/many.txt".
#   -LinkDir    Directory inside the volume the links go in, e.g. "/e".
#   -Links      How many links to make.
#   -KeepImage  `true` to leave .img + .vhd for a follow-on op. Default `false`.
#   -Diag       Directory for attr-list.txt.
#
# Exit code:
#   0 when every link was made and the base record holds an $ATTRIBUTE_LIST
#   1 when a link was refused, the base record holds no $ATTRIBUTE_LIST, or
#     the setup or a record read failed

param(
    [Parameter(Mandatory=$true)] [string]$ImagePath,
    [Parameter(Mandatory=$true)] [string]$Path,
    [Parameter(Mandatory=$true)] [string]$LinkDir,
    [Parameter(Mandatory=$true)] [string]$Links,
    [Parameter(Mandatory=$true)] [string]$Diag,
    [string]$KeepImage = 'false'
)

$ErrorActionPreference = 'Stop'

. "$PSScriptRoot\_lib.ps1"

$KeepImageBool = $false
if ($KeepImage -and $KeepImage.Trim() -ne '') {
    if ($KeepImage -match '^(?i:true|1|yes)$') { $KeepImageBool = $true }
}

$relPath = $Path -replace '^[/\\]+', '' -replace '/', '\'
$relDir = $LinkDir -replace '^[/\\]+', '' -replace '/', '\'
if ($relPath -eq '' -or $relDir -eq '') {
    [Console]::Error.WriteLine("win-link-attr-list: -Path and -LinkDir must name entries under the volume root; got '$Path', '$LinkDir'")
    exit 1
}
$linkCount = [int]$Links
if ($linkCount -lt 1) {
    [Console]::Error.WriteLine("win-link-attr-list: -Links must be at least 1; got '$Links'")
    exit 1
}

Add-Type -TypeDefinition @'
using System;
using System.IO;
using System.Runtime.InteropServices;
using Microsoft.Win32.SafeHandles;

public static class NtfsRecordReader {
    // FSCTL_GET_NTFS_FILE_RECORD: the record as the file system holds it.
    const uint FsctlGetNtfsFileRecord = 0x00090068;

    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    static extern SafeFileHandle CreateFileW(string name, uint access, uint share, IntPtr security,
        uint disposition, uint flags, IntPtr template);
    [DllImport("kernel32.dll", SetLastError = true)]
    static extern bool DeviceIoControl(SafeFileHandle device, uint code, ref long input, int inputSize,
        byte[] output, int outputSize, out int returned, IntPtr overlapped);
    [DllImport("kernel32.dll", SetLastError = true)]
    static extern bool GetFileInformationByHandle(SafeFileHandle file, out ByHandleFileInformation info);

    // Pack = 4: the native struct packs its FILETIMEs on 4 bytes, where
    // a long would be padded to 8 and shift every later field.
    [StructLayout(LayoutKind.Sequential, Pack = 4)]
    struct ByHandleFileInformation {
        public uint Attributes;
        public long Created, Accessed, Written;
        public uint VolumeSerial, SizeHigh, SizeLow, Links, IndexHigh, IndexLow;
    }

    static SafeFileHandle Open(string name, uint access) {
        // FILE_FLAG_BACKUP_SEMANTICS, so a directory opens as well as a file.
        SafeFileHandle handle = CreateFileW(name, access, 7, IntPtr.Zero, 3, 0x02000000, IntPtr.Zero);
        if (handle.IsInvalid) throw new IOException("CreateFileW " + name + ": error " + Marshal.GetLastWin32Error());
        return handle;
    }

    public static long RecordNumber(string file) {
        using (SafeFileHandle handle = Open(file, 0x80)) {
            ByHandleFileInformation info;
            if (!GetFileInformationByHandle(handle, out info))
                throw new IOException("GetFileInformationByHandle " + file + ": error " + Marshal.GetLastWin32Error());
            return (((long)info.IndexHigh << 32) | info.IndexLow) & 0xFFFFFFFFFFFFL;
        }
    }

    // The record numbered `number`, or an error when the volume answers
    // with another (FSCTL_GET_NTFS_FILE_RECORD falls back to the nearest
    // record in use below it).
    public static byte[] Record(string volume, long number) {
        using (SafeFileHandle handle = Open(volume, 0x80000000)) {
            byte[] output = new byte[12 + 65536];
            long input = number;
            int returned;
            if (!DeviceIoControl(handle, FsctlGetNtfsFileRecord, ref input, 8, output, output.Length, out returned, IntPtr.Zero))
                throw new IOException("FSCTL_GET_NTFS_FILE_RECORD " + number + ": error " + Marshal.GetLastWin32Error());
            long got = BitConverter.ToInt64(output, 0) & 0xFFFFFFFFFFFFL;
            if (got != number)
                throw new IOException("FSCTL_GET_NTFS_FILE_RECORD " + number + " answered with record " + got);
            int length = BitConverter.ToInt32(output, 8);
            byte[] record = new byte[length];
            Array.Copy(output, 12, record, 0, length);
            return record;
        }
    }

    // `length` bytes of the volume from byte `offset`; both sector-aligned.
    public static byte[] Read(string volume, long offset, int length) {
        using (SafeFileHandle handle = Open(volume, 0x80000000))
        using (FileStream stream = new FileStream(handle, FileAccess.Read)) {
            stream.Seek(offset, SeekOrigin.Begin);
            byte[] data = new byte[length];
            int done = 0;
            while (done < length) {
                int n = stream.Read(data, done, length - done);
                if (n <= 0) throw new IOException("short read at " + (offset + done) + " of " + volume);
                done += n;
            }
            return data;
        }
    }
}
'@

function U16([byte[]]$b, [int]$o) { [int][BitConverter]::ToUInt16($b, $o) }
function U32([byte[]]$b, [int]$o) { [long][BitConverter]::ToUInt32($b, $o) }
function U64([byte[]]$b, [int]$o) { [BitConverter]::ToUInt64($b, $o) }

# The record with its update sequence applied, unless the volume already
# applied it (no sector ends in the update sequence number).
function Get-Record([string]$Volume, [long]$Number) {
    $rec = [NtfsRecordReader]::Record($Volume, $Number)
    $usaOff = U16 $rec 4
    $usaCount = U16 $rec 6
    $usn = U16 $rec $usaOff
    $tails = @()
    for ($i = 1; $i -lt $usaCount; $i++) {
        $pos = $i * 512 - 2
        if ($pos + 2 -le $rec.Length) { $tails += $pos }
    }
    $pending = $tails.Count -gt 0
    foreach ($pos in $tails) { if ((U16 $rec $pos) -ne $usn) { $pending = $false } }
    if ($pending) {
        for ($i = 1; $i -le $tails.Count; $i++) {
            $rec[$tails[$i - 1]] = $rec[$usaOff + 2 * $i]
            $rec[$tails[$i - 1] + 1] = $rec[$usaOff + 2 * $i + 1]
        }
    }
    return ,$rec
}

# Every attribute in a record: type, residency, id, name, the $FILE_NAME a
# resident 0x30 carries, and the record's bytes for 0x20 to read.
function Get-Attributes([byte[]]$Rec) {
    $used = U32 $Rec 0x18
    $off = U16 $Rec 0x14
    $attrs = @()
    while ($off + 8 -le $used) {
        $type = U32 $Rec $off
        if ($type -eq 0xFFFFFFFF) { break }
        $alen = U32 $Rec ($off + 4)
        if ($alen -lt 16 -or $off + $alen -gt $used) { throw ("attribute at {0} runs past the record ({1} bytes)" -f $off, $alen) }
        $nonres = $Rec[$off + 8]
        $nlen = $Rec[$off + 9]
        $name = ''
        if ($nlen -gt 0) { $name = [Text.Encoding]::Unicode.GetString($Rec, $off + (U16 $Rec ($off + 10)), 2 * $nlen) }
        $fileName = ''
        if ($type -eq 0x30 -and $nonres -eq 0) {
            $v = $off + (U16 $Rec ($off + 0x14))
            $fileName = [Text.Encoding]::Unicode.GetString($Rec, $v + 0x42, 2 * $Rec[$v + 0x40])
        }
        $attrs += [pscustomobject]@{
            Type = $type; NonResident = $nonres; Id = (U16 $Rec ($off + 14)); Name = $name
            FileName = $fileName; Offset = $off; Length = $alen
        }
        $off += $alen
    }
    return ,$attrs
}

function Format-Types($Attrs) { (($Attrs | ForEach-Object { '0x{0:x}' -f $_.Type }) -join ',') }

# The $ATTRIBUTE_LIST's value, read from its clusters when it is not
# resident.
function Get-ListValue([string]$Volume, [byte[]]$Rec, $Attr) {
    $off = $Attr.Offset
    if ($Attr.NonResident -eq 0) {
        $vlen = U32 $Rec ($off + 0x10)
        $value = New-Object byte[] $vlen
        [Array]::Copy($Rec, $off + (U16 $Rec ($off + 0x14)), $value, 0, $vlen)
        return ,$value
    }
    $boot = [NtfsRecordReader]::Read($Volume, 0, 512)
    $spc = [int]$boot[0x0D]
    if ($spc -gt 0x80) { $spc = [int][Math]::Pow(2, 256 - $spc) }
    $cluster = (U16 $boot 0x0B) * $spc
    $size = [long](U64 $Rec ($off + 0x30))
    $value = New-Object byte[] $size
    $filled = 0
    $p = $off + (U16 $Rec ($off + 0x20))
    $lcn = [long]0
    while ($Rec[$p] -ne 0 -and $filled -lt $size) {
        $lenBytes = $Rec[$p] -band 0x0F
        $offBytes = $Rec[$p] -shr 4
        $runLen = [long]0
        for ($i = 0; $i -lt $lenBytes; $i++) { $runLen = $runLen -bor ([long]$Rec[$p + 1 + $i] -shl (8 * $i)) }
        $delta = [long]0
        for ($i = 0; $i -lt $offBytes; $i++) { $delta = $delta -bor ([long]$Rec[$p + 1 + $lenBytes + $i] -shl (8 * $i)) }
        if ($offBytes -gt 0 -and ($Rec[$p + $lenBytes + $offBytes] -band 0x80)) { $delta -= ([long]1 -shl (8 * $offBytes)) }
        $lcn += $delta
        $bytes = [NtfsRecordReader]::Read($Volume, $lcn * $cluster, [int]($runLen * $cluster))
        $take = [Math]::Min([long]$bytes.Length, $size - $filled)
        [Array]::Copy($bytes, 0, $value, $filled, $take)
        $filled += $take
        $p += 1 + $lenBytes + $offBytes
    }
    return ,$value
}

New-Item -ItemType Directory -Path $Diag -Force | Out-Null
$Vhd = Get-VhdPathFor -ImagePath $ImagePath
$report = "$Diag\attr-list.txt"
$lines = @()

try {
    $state = Initialize-VhdFromImg -ImagePath $ImagePath -Diag $Diag
    $letter = Mount-VhdAndGetLetter -Vhd $state.Vhd
    $volume = "\\.\${letter}:"

    $target = "${letter}:\$relPath"
    $dir = "${letter}:\$relDir"
    New-Item -ItemType Directory -Path $dir -Force | Out-Null
    [IO.File]::WriteAllBytes($target, [byte[]](,0x6d * 64))
    $number = [NtfsRecordReader]::RecordNumber($target)
    $lines += "record=$number"

    for ($k = 0; $k -lt $linkCount; $k++) {
        $link = "$dir\many-$k.txt"
        try {
            New-Item -ItemType HardLink -Path $link -Target $target | Out-Null
        } catch {
            throw "Windows refused link $k ($link): $($_.Exception.Message)"
        }
        $rec = Get-Record $volume $number
        $attrs = Get-Attributes $rec
        $list = $attrs | Where-Object { $_.Type -eq 0x20 } | Select-Object -First 1
        $kind = 'none'
        if ($list) { $kind = @('resident', 'nonresident')[[int]($list.NonResident -ne 0)] }
        $lines += ("link={0} base_used={1} attr_list={2} base_types={3}" -f $k, (U32 $rec 0x18), $kind, (Format-Types $attrs))
    }

    $rec = Get-Record $volume $number
    $attrs = Get-Attributes $rec
    $lines += ("base used={0} allocated={1}" -f (U32 $rec 0x18), (U32 $rec 0x1C))
    foreach ($a in $attrs) {
        $lines += ("base type=0x{0:x} id={1} nonresident={2} length={3} name={4} file_name={5}" -f $a.Type, $a.Id, $a.NonResident, $a.Length, $a.Name, $a.FileName)
    }
    $list = $attrs | Where-Object { $_.Type -eq 0x20 } | Select-Object -First 1
    if (-not $list) {
        throw "Windows left $Path with no `$ATTRIBUTE_LIST in its base record after $linkCount links; see $report"
    }

    $value = Get-ListValue $volume $rec $list
    $records = @()
    $moved = @()
    $p = 0
    while ($p + 26 -le $value.Length) {
        $type = U32 $value $p
        $entryLen = U16 $value ($p + 4)
        if ($entryLen -lt 26) { break }
        $nlen = $value[$p + 6]
        $name = ''
        if ($nlen -gt 0) { $name = [Text.Encoding]::Unicode.GetString($value, $p + $value[$p + 7], 2 * $nlen) }
        $where = [BitConverter]::ToInt64($value, $p + 16) -band 0xFFFFFFFFFFFF
        $lines += ("list type=0x{0:x} record={1} vcn={2} id={3} name={4}" -f $type, $where, (U64 $value ($p + 8)), (U16 $value ($p + 24)), $name)
        if ($where -ne $number) {
            if ($records -notcontains $where) { $records += $where }
            $t = '0x{0:x}' -f $type
            if ($moved -notcontains $t) { $moved += $t }
        }
        $p += $entryLen
    }
    foreach ($r in $records) {
        foreach ($a in (Get-Attributes (Get-Record $volume $r))) {
            $lines += ("ext record={0} type=0x{1:x} id={2} nonresident={3} length={4} name={5} file_name={6}" -f $r, $a.Type, $a.Id, $a.NonResident, $a.Length, $a.Name, $a.FileName)
        }
    }
    $lines += "moved=$($moved -join ',')"
    $lines -join "`r`n" | Out-File $report -Encoding UTF8

    Write-Host ("win-link-attr-list: record {0} took {1} links; its `$ATTRIBUTE_LIST is {2}; moved out of the base record: {3}" -f
        $number, $linkCount, @('resident', 'nonresident')[[int]($list.NonResident -ne 0)], ($moved -join ','))
    exit 0
} catch {
    if ($lines.Count -gt 0) { $lines -join "`r`n" | Out-File $report -Encoding UTF8 }
    [Console]::Error.WriteLine("win-link-attr-list: $($_.Exception.Message)")
    exit 1
} finally {
    Dismount-VhdAndCleanup -Vhd $Vhd -ImagePath $ImagePath -KeepImage $KeepImageBool
}
