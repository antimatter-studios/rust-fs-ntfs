# Builds ntfs-salvage-streams.img: the shapes a salvage read has to follow
# out of one file record (#1379), each made by Windows and reported by
# Windows, so tests/salvage_streams_windows.rs grades PrimitiveAPI against
# the platform rather than against this crate's own writer.
#
#   /ads.bin          an unnamed stream and 84 named streams, more than its
#                     base record holds, so Windows moves streams to
#                     extension records through an $ATTRIBUTE_LIST; four of
#                     them are non-resident
#   /links.bin        one file with 24 hard links whose long names cannot
#                     all fit its base record
#   /fragmented.bin   a file written in 1200 single-cluster appends, each
#                     followed by a one-cluster filler file, so its run list
#                     outgrows the base record and continues in an extension
#                     record at a non-zero starting VCN
#   /packed/list.bin  an LZNT1-compressed file whose units mix compressed,
#                     stored and all-zero units, enough that its run list
#                     continues in an extension record
#   /packed/small.txt an LZNT1-compressed file held in one record
#
# ntfs-salvage-streams.tsv is what Windows reports, one row per fact:
#
#   stream  <path>  <record>  <stream name, empty for unnamed>  <length>  <sha256>
#   link    <path>  <record>  <one name fsutil hardlink list reports>
#   extents <path>  <record>  <extents fsutil file queryextents reports>
#
# <record> is the low 48 bits of the file reference Windows gives the open
# handle. Stream bytes are read back through Windows' own filesystem.
#
# Runs on Windows PowerShell 5.1 and PowerShell 7, elevated, from the
# repository root, with rust-img-vhd on PATH. build-windows-native-read-
# fixtures.ps1 runs it too, so CI builds it beside the other fixtures.

$ErrorActionPreference = 'Stop'

. "$PSScriptRoot\..\scripts\fs-windows-test-harness\_lib.ps1"
. "$PSScriptRoot\windows-fixture.ps1"

if (-not ('SalvageFixtureFiles' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.IO;
using System.Runtime.InteropServices;
using Microsoft.Win32.SafeHandles;
public static class SalvageFixtureFiles {
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    static extern SafeFileHandle CreateFileW(string path, uint access, uint share,
        IntPtr security, uint disposition, uint flags, IntPtr template);
    [DllImport("kernel32.dll", SetLastError = true)]
    static extern bool GetFileInformationByHandle(SafeFileHandle h, out BY_HANDLE_FILE_INFORMATION info);
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    static extern bool CreateHardLinkW(string link, string existing, IntPtr security);
    [StructLayout(LayoutKind.Sequential)]
    struct BY_HANDLE_FILE_INFORMATION {
        // FILETIMEs as two uints each: a long would be 8-aligned and shift the rest.
        public uint Attributes; public uint C0, C1, A0, A1, W0, W1;
        public uint Volume; public uint SizeHigh; public uint SizeLow; public uint Links;
        public uint IndexHigh; public uint IndexLow;
    }
    const uint GENERIC_READ = 0x80000000, GENERIC_WRITE = 0x40000000;
    const uint SHARE_ALL = 7, CREATE_ALWAYS = 2, OPEN_EXISTING = 3, OPEN_ALWAYS = 4;
    const uint WRITE_THROUGH = 0x80000000, BACKUP_SEMANTICS = 0x02000000;

    static FileStream Open(string path, uint access, uint disposition, uint flags) {
        var h = CreateFileW(path, access, SHARE_ALL, IntPtr.Zero, disposition, flags, IntPtr.Zero);
        if (h.IsInvalid) throw new Win32Exception(Marshal.GetLastWin32Error(), path);
        return new FileStream(h, access == GENERIC_READ ? FileAccess.Read : FileAccess.ReadWrite);
    }
    // path may name a stream ("X:\file:name"), which .NET Framework's own
    // FileStream refuses.
    public static void Write(string path, byte[] bytes) {
        using (var s = Open(path, GENERIC_READ | GENERIC_WRITE, CREATE_ALWAYS, 0)) {
            s.Write(bytes, 0, bytes.Length);
            s.Flush(true);
        }
    }
    public static void Append(string path, byte[] bytes) {
        using (var s = Open(path, GENERIC_READ | GENERIC_WRITE, OPEN_ALWAYS, WRITE_THROUGH)) {
            s.Seek(0, SeekOrigin.End);
            s.Write(bytes, 0, bytes.Length);
            s.Flush(true);
        }
    }
    public static byte[] Read(string path) {
        using (var s = Open(path, GENERIC_READ, OPEN_EXISTING, 0))
        using (var m = new MemoryStream()) {
            s.CopyTo(m);
            return m.ToArray();
        }
    }
    public static ulong Record(string path) {
        using (var h = CreateFileW(path, 0x80, SHARE_ALL, IntPtr.Zero, OPEN_EXISTING, BACKUP_SEMANTICS, IntPtr.Zero)) {
            if (h.IsInvalid) throw new Win32Exception(Marshal.GetLastWin32Error(), path);
            BY_HANDLE_FILE_INFORMATION info;
            if (!GetFileInformationByHandle(h, out info)) throw new Win32Exception(Marshal.GetLastWin32Error(), path);
            return (((ulong)info.IndexHigh << 32) | info.IndexLow) & 0x0000FFFFFFFFFFFFUL;
        }
    }
    public static void Link(string link, string existing) {
        if (!CreateHardLinkW(link, existing, IntPtr.Zero)) throw new Win32Exception(Marshal.GetLastWin32Error(), link);
    }
}
'@
}

# Deterministic bytes, so a rebuilt fixture holds the same content.
$script:salvageRandom = New-Object System.Random 1379
function New-SalvageBytes([int]$Length) {
    $bytes = New-Object byte[] $Length
    $script:salvageRandom.NextBytes($bytes)
    , $bytes
}

function Get-SalvageSha256([byte[]]$Bytes) {
    $sha = [System.Security.Cryptography.SHA256]::Create()
    try { ([BitConverter]::ToString($sha.ComputeHash($Bytes)) -replace '-', '').ToLowerInvariant() }
    finally { $sha.Dispose() }
}

function Get-SalvageStreamRows([string]$Root, [string]$VolumePath, [string[]]$Names) {
    $file = Join-Path $Root $VolumePath.TrimStart('/').Replace('/', '\')
    $record = [SalvageFixtureFiles]::Record($file)
    foreach ($name in $Names) {
        $target = if ($name) { "${file}:$name" } else { $file }
        $bytes = [SalvageFixtureFiles]::Read($target)
        "stream`t$VolumePath`t$record`t$name`t$($bytes.Length)`t$(Get-SalvageSha256 $bytes)"
    }
}

function Get-SalvageExtentCount([string]$File) {
    $lines = & fsutil.exe file queryextents $File
    if ($LASTEXITCODE -ne 0) { throw "fsutil file queryextents $File exited $LASTEXITCODE" }
    @($lines | Where-Object { $_ -match '^VCN:' }).Count
}

$salvageReport = [IO.Path]::GetFullPath('test-disks/ntfs-salvage-streams.tsv')
[IO.File]::WriteAllText($salvageReport, '')

New-WindowsFixture -ImagePath 'test-disks/ntfs-salvage-streams.img' -Label 'SALVAGE' -Populate {
    param($root)
    $rows = New-Object System.Collections.Generic.List[string]

    # Named streams beyond the base record.
    $ads = Join-Path $root 'ads.bin'
    [SalvageFixtureFiles]::Write($ads, (New-SalvageBytes 5000))
    $names = @('')
    for ($i = 0; $i -lt 80; $i++) {
        $name = 'ads-{0:D3}' -f $i
        [SalvageFixtureFiles]::Write("${ads}:$name", (New-SalvageBytes ((($i * 37) % 300) + 1)))
        $names += $name
    }
    for ($i = 0; $i -lt 4; $i++) {
        $name = 'big-{0}' -f $i
        [SalvageFixtureFiles]::Write("${ads}:$name", (New-SalvageBytes (50000 + $i * 4099)))
        $names += $name
    }
    $rows.AddRange([string[]]@(Get-SalvageStreamRows $root '/ads.bin' $names))

    # Hard links whose names overflow the base record.
    $links = Join-Path $root 'links.bin'
    [SalvageFixtureFiles]::Write($links, (New-SalvageBytes 3000))
    $linkDir = Join-Path $root 'links'
    New-Item -ItemType Directory -Path $linkDir | Out-Null
    for ($i = 0; $i -lt 24; $i++) {
        $leaf = ('link-{0:D2}-' -f $i) + ('n' * 180)
        [SalvageFixtureFiles]::Link((Join-Path $linkDir $leaf), $links)
    }
    $rows.AddRange([string[]]@(Get-SalvageStreamRows $root '/links.bin' @('')))
    $linkRecord = [SalvageFixtureFiles]::Record($links)
    $listed = & fsutil.exe hardlink list $links
    if ($LASTEXITCODE -ne 0) { throw "fsutil hardlink list exited $LASTEXITCODE" }
    foreach ($name in @($listed | Where-Object { $_ })) {
        $rows.Add("link`t/links.bin`t$linkRecord`t$($name.Trim())")
    }

    # A run list that continues in an extension record.
    $fragmented = Join-Path $root 'fragmented.bin'
    $filler = Join-Path $root 'filler'
    New-Item -ItemType Directory -Path $filler | Out-Null
    for ($i = 0; $i -lt 1200; $i++) {
        [SalvageFixtureFiles]::Append($fragmented, (New-SalvageBytes 4096))
        [SalvageFixtureFiles]::Append((Join-Path $filler ('f{0:D4}' -f $i)), (New-SalvageBytes 4096))
    }
    $rows.AddRange([string[]]@(Get-SalvageStreamRows $root '/fragmented.bin' @('')))
    $extents = Get-SalvageExtentCount $fragmented
    if ($extents -lt 600) { throw "fragmented.bin has $extents extents; the fixture needs far more" }
    $rows.Add("extents`t/fragmented.bin`t$([SalvageFixtureFiles]::Record($fragmented))`t$extents")

    # LZNT1 compression, with and without an attribute list.
    $packed = Join-Path $root 'packed'
    New-Item -ItemType Directory -Path $packed | Out-Null
    & compact.exe /C /Q $packed | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "compact.exe /C $packed exited $LASTEXITCODE" }
    $unit = 65536
    $list = Join-Path $packed 'list.bin'
    $units = 600
    $content = New-Object byte[] ($units * $unit + 777)
    for ($u = 0; $u -lt $units; $u++) {
        $random = switch ($u % 5) { 0 { 0 } 1 { $unit } default { 24576 } }
        if ($random -gt 0) {
            [Array]::Copy((New-SalvageBytes $random), 0, $content, $u * $unit, $random)
        }
    }
    [Array]::Copy((New-SalvageBytes 777), 0, $content, $units * $unit, 777)
    [SalvageFixtureFiles]::Write($list, $content)
    $small = Join-Path $packed 'small.txt'
    $text = [Text.Encoding]::ASCII.GetBytes(('salvage the compressed record ' * 7000))
    [SalvageFixtureFiles]::Write($small, $text)
    foreach ($target in @($list, $small)) {
        if (((Get-Item -LiteralPath $target -Force).Attributes -band [IO.FileAttributes]::Compressed) -eq 0) {
            throw "Windows did not compress $target"
        }
    }
    $rows.AddRange([string[]]@(Get-SalvageStreamRows $root '/packed/list.bin' @('')))
    $rows.AddRange([string[]]@(Get-SalvageStreamRows $root '/packed/small.txt' @('')))
    $rows.Add("extents`t/packed/list.bin`t$([SalvageFixtureFiles]::Record($list))`t$(Get-SalvageExtentCount $list)")

    [IO.File]::WriteAllText($salvageReport, (($rows -join "`n") + "`n"),
        (New-Object System.Text.UTF8Encoding($false)))
}
