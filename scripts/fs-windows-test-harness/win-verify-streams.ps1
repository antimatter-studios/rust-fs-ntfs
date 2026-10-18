# scripts/fs-windows-test-harness/win-verify-streams.ps1 -- have Windows
# read back the named streams mac-write-streams.sh (or win-write-streams.ps1)
# gave one file, and fail unless they are exactly the ones expected, each
# holding what it should.
#
# The file carries s1..s<Count>, less every -DeleteEvery-th one when that
# is above 0, and stream i holds "stream <i> " repeated to -SizeBytes. A
# stream this crate moved to an extension record behind an
# $ATTRIBUTE_LIST is found by Windows only if the list names it where it
# is (#480), so a missing, extra or misplaced stream fails here even when
# chkdsk has nothing to say.
#
# Args:
#   -ImagePath    Path on the VM to the .img file.
#   -Path         File path inside the volume, e.g. "/many.bin".
#   -Count        Positive integer; the highest stream number written.
#   -SizeBytes    Bytes per stream (> 0).
#   -DeleteEvery  Non-negative integer; every k-th stream was deleted. 0: none.
#   -KeepImage    `true` to leave .img + .vhd for a follow-on op. Default `false`.
#   -Diag         Directory for verify-streams-result.txt + verify-streams-error.txt.
#
# Exit code:
#   0 when every expected stream, and no other, reads back as written
#   1 otherwise (logged to verify-streams-error.txt)
#   2 for arg errors

param(
    [Parameter(Mandatory=$true)] [string]$ImagePath,
    [Parameter(Mandatory=$true)] [string]$Path,
    [Parameter(Mandatory=$true)] [string]$Count,
    [Parameter(Mandatory=$true)] [string]$SizeBytes,
    [Parameter(Mandatory=$true)] [string]$DeleteEvery,
    [Parameter(Mandatory=$true)] [string]$Diag,
    [string]$KeepImage = 'false'
)

$ErrorActionPreference = 'Stop'

. "$PSScriptRoot\_lib.ps1"

# Windows PowerShell 5.1's -Stream provider handles files only. Use the
# documented Win32 stream API for directories, including a backup handle
# when opening their streams. See docs/provenance.md for the API references.
Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.ComponentModel;
using System.IO;
using System.Runtime.InteropServices;
using Microsoft.Win32.SafeHandles;
public static class WindowsDirectoryStreams {
    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    public struct StreamData {
        public long Size;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 296)] public string Name;
    }
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    static extern IntPtr FindFirstStreamW(string file, int level, out StreamData data, uint flags);
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    static extern bool FindNextStreamW(IntPtr handle, out StreamData data);
    [DllImport("kernel32.dll")] static extern bool FindClose(IntPtr handle);
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    static extern SafeFileHandle CreateFileW(string file, uint access, uint share,
        IntPtr security, uint disposition, uint flags, IntPtr template);
    public static string[] Names(string file) {
        var names = new List<string>();
        StreamData data;
        IntPtr handle = FindFirstStreamW(file, 0, out data, 0);
        if (handle == new IntPtr(-1)) {
            int error = Marshal.GetLastWin32Error();
            if (error == 38) return names.ToArray(); // ERROR_HANDLE_EOF
            throw new Win32Exception(error);
        }
        try {
            do {
                if (data.Name != "::$DATA") {
                    if (!data.Name.StartsWith(":" ) || !data.Name.EndsWith(":$DATA"))
                        throw new IOException("Unexpected stream name: " + data.Name);
                    names.Add(data.Name.Substring(1, data.Name.Length - 7));
                }
            } while (FindNextStreamW(handle, out data));
            int error = Marshal.GetLastWin32Error();
            if (error != 38) throw new Win32Exception(error);
        } finally { FindClose(handle); }
        return names.ToArray();
    }
    public static byte[] Read(string file) {
        using (var handle = CreateFileW(file, 0x80000000, 7, IntPtr.Zero, 3, 0x02000000, IntPtr.Zero)) {
            if (handle.IsInvalid) throw new Win32Exception(Marshal.GetLastWin32Error());
            using (var input = new FileStream(handle, FileAccess.Read))
            using (var output = new MemoryStream()) {
                input.CopyTo(output);
                return output.ToArray();
            }
        }
    }
}
'@

$KeepImageBool = $false
if ($KeepImage -and $KeepImage.Trim() -ne '') {
    if ($KeepImage -match '^(?i:true|1|yes)$') { $KeepImageBool = $true }
}

$countInt = 0
if (-not [int]::TryParse($Count, [ref]$countInt) -or $countInt -lt 1) {
    [Console]::Error.WriteLine("invalid -Count: '$Count' (expected a positive integer)")
    exit 2
}
$sizeInt = 0
if (-not [int]::TryParse($SizeBytes, [ref]$sizeInt) -or $sizeInt -lt 1) {
    [Console]::Error.WriteLine("invalid -SizeBytes: '$SizeBytes' (expected a positive integer)")
    exit 2
}
$everyInt = 0
if (-not [int]::TryParse($DeleteEvery, [ref]$everyInt) -or $everyInt -lt 0) {
    [Console]::Error.WriteLine("invalid -DeleteEvery: '$DeleteEvery' (expected a non-negative integer)")
    exit 2
}

$relPath = $Path -replace '^[/\\]+', ''
if ($relPath -eq '') {
    [Console]::Error.WriteLine("win-verify-streams: -Path must name a file under the volume root; got '$Path'")
    exit 2
}

New-Item -ItemType Directory -Path $Diag -Force | Out-Null
$Vhd = Get-VhdPathFor -ImagePath $ImagePath

try {
    $state = Initialize-VhdFromImg -ImagePath $ImagePath -Diag $Diag
    $letter = Mount-VhdAndGetLetter -Vhd $state.Vhd

    $target = "${letter}:\$relPath"
    try {
        $expected = @()
        for ($i = 1; $i -le $countInt; $i++) {
            if ($everyInt -gt 0 -and ($i % $everyInt) -eq 0) { continue }
            $expected += "s$i"
        }
        $isDirectory = Test-Path -LiteralPath $target -PathType Container
        $listed = if ($isDirectory) { @([WindowsDirectoryStreams]::Names($target)) } else {
            @(Get-Item -LiteralPath $target -Stream * |
                Where-Object { $_.Stream -ne ':$DATA' } |
                ForEach-Object { $_.Stream })
        }
        $missing = @($expected | Where-Object { $listed -notcontains $_ })
        $extra = @($listed | Where-Object { $expected -notcontains $_ })
        if ($missing.Count -gt 0 -or $extra.Count -gt 0) {
            throw "Windows lists $($listed.Count) streams on $target, want $($expected.Count): missing [$($missing -join ', ')], unexpected [$($extra -join ', ')]"
        }
        foreach ($name in $expected) {
            $i = [int]$name.Substring(1)
            $unit = [System.Text.Encoding]::UTF8.GetBytes("stream $i ")
            $got = if ($isDirectory) { [WindowsDirectoryStreams]::Read("${target}:$name") } else {
                [byte[]](Get-Content -LiteralPath $target -Stream $name -Encoding Byte -Raw)
            }
            if ($got.Length -ne $sizeInt) {
                throw "stream $name of $target is $($got.Length) bytes, want $sizeInt"
            }
            for ($b = 0; $b -lt $sizeInt; $b++) {
                if ($got[$b] -ne $unit[$b % $unit.Length]) {
                    throw "stream $name of $target differs at byte $b"
                }
            }
        }
        "Windows read back the $($expected.Count) streams of $sizeInt bytes on $target" |
            Out-File "$Diag\verify-streams-result.txt" -Encoding UTF8
    } catch {
        $_.Exception.Message | Out-File "$Diag\verify-streams-error.txt" -Encoding UTF8
        throw
    }

    exit 0
} finally {
    Dismount-VhdAndCleanup -Vhd $Vhd -ImagePath $ImagePath -KeepImage $KeepImageBool
}
