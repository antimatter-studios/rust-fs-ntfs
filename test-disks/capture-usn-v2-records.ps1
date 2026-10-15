# Captures real USN_RECORD_V2 records written by Windows, for the decoder's
# cross-validation (#609).
#
# Formats a disposable NTFS volume (a VHD attached from an image in the
# runner's temp directory), creates a change journal on it with `fsutil usn
# createjournal`, and creates a marker file with a non-ASCII name there. The
# runner's own volumes are never touched: their journals may be inactive, and
# a system journal is not ours to create or reset. The script then reads the
# change journal with FSCTL_READ_USN_JOURNAL (READ_USN_JOURNAL_DATA_V0, which Windows answers with
# version 2.0 records) from the USN that FSCTL_QUERY_USN_JOURNAL reported
# before the file was created. Every returned record naming the marker is
# kept, byte for byte, in test-disks/usn-v2-capture.bin.
#
# The oracle is Windows itself, not this script's reading of the bytes:
# test-disks/usn-v2-capture.tsv records, per record, its offset and length in
# the capture and the marker's file ID as `fsutil file queryfileid` reports
# it, and the SHA-256 of the capture. tests/usn_windows_capture.rs decodes the
# capture and must agree with fsutil.
#
# Run from the repository root on an elevated Windows host. rust-img-vhd must be
# on PATH; CI installs the pinned rust-img-vhd release before invoking this.

$ErrorActionPreference = 'Stop'

. "$PSScriptRoot\..\scripts\fs-windows-test-harness\_lib.ps1"

Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
using Microsoft.Win32.SafeHandles;
public static class UsnCapture {
    [DllImport("kernel32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
    static extern SafeFileHandle CreateFileW(string name, uint access, uint share,
        IntPtr sa, uint disposition, uint flags, IntPtr template);
    [DllImport("kernel32.dll", SetLastError = true)]
    static extern bool DeviceIoControl(SafeFileHandle h, uint code, byte[] inBuf,
        int inLen, byte[] outBuf, int outLen, out int returned, IntPtr ov);
    const uint FSCTL_QUERY_USN_JOURNAL = 0x000900F4;
    const uint FSCTL_READ_USN_JOURNAL = 0x000900BB;

    public static SafeFileHandle Open(string drive) {
        var h = CreateFileW(@"\\.\" + drive, 0x80000000, 3, IntPtr.Zero, 3, 0, IntPtr.Zero);
        if (h.IsInvalid) throw new System.ComponentModel.Win32Exception();
        return h;
    }

    // Returns { UsnJournalID, NextUsn } from USN_JOURNAL_DATA.
    public static long[] Query(SafeFileHandle h) {
        var o = new byte[80]; int n;
        if (!DeviceIoControl(h, FSCTL_QUERY_USN_JOURNAL, null, 0, o, o.Length, out n, IntPtr.Zero))
            throw new System.ComponentModel.Win32Exception();
        return new long[] { BitConverter.ToInt64(o, 0), BitConverter.ToInt64(o, 16) };
    }

    // One FSCTL_READ_USN_JOURNAL call; returns the output buffer, whose first
    // eight bytes are the next USN and the rest whole records.
    public static byte[] Read(SafeFileHandle h, long journalId, long startUsn) {
        var i = new byte[40];
        BitConverter.GetBytes(startUsn).CopyTo(i, 0);
        BitConverter.GetBytes(0xFFFFFFFFu).CopyTo(i, 8);
        BitConverter.GetBytes(journalId).CopyTo(i, 32);
        var o = new byte[65536]; int n;
        if (!DeviceIoControl(h, FSCTL_READ_USN_JOURNAL, i, i.Length, o, o.Length, out n, IntPtr.Zero))
            throw new System.ComponentModel.Win32Exception();
        var r = new byte[n]; Array.Copy(o, r, n); return r;
    }
}
'@

$tempRoot = if ($env:RUNNER_TEMP) { $env:RUNNER_TEMP } else { [System.IO.Path]::GetTempPath() }
$name = "usn-v2-$([guid]::NewGuid().ToString('N').Substring(0, 8))-é.txt"
$out = [System.IO.Path]::GetFullPath('test-disks/usn-v2-capture.bin')
$manifest = [System.IO.Path]::GetFullPath('test-disks/usn-v2-capture.tsv')
$image = Join-Path $tempRoot 'usn-v2-capture-volume.img'
$diag = Join-Path $tempRoot 'usn-v2-capture-volume'
New-Item -ItemType Directory -Path $diag -Force | Out-Null
Remove-Item -LiteralPath $image -Force -ErrorAction SilentlyContinue
$stream = [System.IO.File]::Create($image)
try { $stream.SetLength(256MB) } finally { $stream.Close() }

$rows = @()
$state = Initialize-VhdFromImg -ImagePath $image -Diag $diag
try {
$letter = Mount-VhdAndGetLetter -Vhd $state.Vhd
Format-Volume -DriveLetter $letter -FileSystem NTFS -NewFileSystemLabel 'USNCAPTURE' `
    -Force -Confirm:$false | Out-Null
$drive = "${letter}:"
$o = & fsutil.exe usn createjournal m=8388608 a=1048576 $drive 2>&1
if ($LASTEXITCODE -ne 0) { throw "fsutil usn createjournal on $drive failed: $o" }
$marker = Join-Path "$drive\" $name

$h = [UsnCapture]::Open($drive)
try {
    $q = [UsnCapture]::Query($h)
    $journalId = $q[0]; $usn = $q[1]
    Set-Content -LiteralPath $marker -Value 'usn capture' -NoNewline
    Add-Content -LiteralPath $marker -Value ' appended' -NoNewline
    $fileId = ((fsutil file queryfileid $marker) -replace '^.*:\s*', '').Trim()
    if ($LASTEXITCODE -ne 0) { throw "fsutil file queryfileid $marker failed" }

    $kept = New-Object System.IO.MemoryStream
    for ($pass = 0; $pass -lt 64; $pass++) {
        $buf = [UsnCapture]::Read($h, $journalId, $usn)
        if ($buf.Length -le 8) { break }
        $usn = [BitConverter]::ToInt64($buf, 0)
        $pos = 8
        while ($pos -lt $buf.Length) {
            $len = [BitConverter]::ToUInt32($buf, $pos)
            $nameLen = [BitConverter]::ToUInt16($buf, $pos + 56)
            $nameOff = [BitConverter]::ToUInt16($buf, $pos + 58)
            $recName = [System.Text.Encoding]::Unicode.GetString($buf, $pos + $nameOff, $nameLen)
            if ($recName -eq $name) {
                $rows += "$($kept.Length)`t$len`t$fileId`t$name"
                $kept.Write($buf, $pos, $len)
            }
            $pos += $len
        }
    }
} finally {
    $h.Dispose()
}
} finally {
    Get-DiskImage -ImagePath $state.Vhd -ErrorAction SilentlyContinue |
        Where-Object Attached |
        Dismount-DiskImage -ErrorAction SilentlyContinue | Out-Null
    Remove-Item -LiteralPath $state.Vhd -Force -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $image -Force -ErrorAction SilentlyContinue
}
if ($rows.Count -eq 0) { throw "no USN record named $name was returned from $drive" }

[System.IO.File]::WriteAllBytes($out, $kept.ToArray())
$sha = (Get-FileHash -Algorithm SHA256 -LiteralPath $out).Hash.ToLowerInvariant()
$lines = @("# sha256`t$sha") + $rows
[System.IO.File]::WriteAllLines($manifest, $lines, (New-Object System.Text.UTF8Encoding $false))
Write-Host "captured $($rows.Count) USN_RECORD_V2 records for $name; sha256 $sha"
