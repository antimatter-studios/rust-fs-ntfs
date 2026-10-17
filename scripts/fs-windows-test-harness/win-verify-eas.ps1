# scripts/fs-windows-test-harness/win-verify-eas.ps1 -- have Windows read
# back the extended attributes mac-write-eas.sh gave one file, and fail
# unless they are exactly the ones expected, each holding what it should.
#
# The file carries USER.E1..USER.E<Count>, less every -DeleteEvery-th one
# when that is above 0, and EA i holds "ea <i> " repeated to -SizeBytes.
# The EAs are read with NtQueryEaFile, the call every Windows EA API
# reaches. An $EA this crate moved to an extension record behind an
# $ATTRIBUTE_LIST is found by Windows only if the list names it where it
# is (#660, part of #480), so a missing, extra or misplaced EA fails here
# even when chkdsk has nothing to say.
#
# Args:
#   -ImagePath    Path on the VM to the .img file.
#   -Path         File path inside the volume, e.g. "/many.bin".
#   -Count        Positive integer; the highest EA number written.
#   -SizeBytes    Bytes per EA value (> 0).
#   -DeleteEvery  Non-negative integer; every k-th EA was removed. 0: none.
#   -KeepImage    `true` to leave .img + .vhd for a follow-on op. Default `false`.
#   -Diag         Directory for verify-eas-result.txt + verify-eas-error.txt.
#
# Exit code:
#   0 when every expected EA, and no other, reads back as written
#   1 otherwise (logged to verify-eas-error.txt)
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
    [Console]::Error.WriteLine("win-verify-eas: -Path must name a file under the volume root; got '$Path'")
    exit 2
}

Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
using Microsoft.Win32.SafeHandles;

public static class WindowsEaQuery {
    [StructLayout(LayoutKind.Sequential)]
    public struct IoStatusBlock { public IntPtr Status; public IntPtr Information; }

    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    public static extern SafeFileHandle CreateFileW(
        string name, uint access, uint share, IntPtr security,
        uint disposition, uint flags, IntPtr template);

    [DllImport("ntdll.dll")]
    public static extern int NtQueryEaFile(
        SafeFileHandle handle, out IoStatusBlock status, byte[] buffer, uint length,
        bool returnSingleEntry, IntPtr eaList, uint eaListLength, IntPtr eaIndex,
        bool restartScan);
}
'@

# Every EA on $file, as an ordered name -> value map, read in one
# NtQueryEaFile call: a file's EAs total at most 64 KiB, so a 128 KiB buffer
# holds them all.
function Read-WindowsEas([string]$file) {
    # FILE_READ_EA | SYNCHRONIZE, OPEN_EXISTING: a synchronous handle, which
    # NtQueryEaFile needs.
    $handle = [WindowsEaQuery]::CreateFileW($file, [uint32]0x100008, [uint32]7, [IntPtr]::Zero,
        [uint32]3, [uint32]0x80, [IntPtr]::Zero)
    if ($handle.IsInvalid) {
        throw "open $file for EAs: Win32 error $([System.Runtime.InteropServices.Marshal]::GetLastWin32Error())"
    }
    $eas = [ordered]@{}
    try {
        $buf = New-Object byte[] (128 * 1024)
        $status = New-Object WindowsEaQuery+IoStatusBlock
        $rc = [WindowsEaQuery]::NtQueryEaFile($handle, [ref]$status, $buf, [uint32]$buf.Length,
            $false, [IntPtr]::Zero, [uint32]0, [IntPtr]::Zero, $true)
        # STATUS_NO_EAS_ON_FILE: the file has none.
        if ($rc -eq [int]0xC0000052) { return $eas }
        if ($rc -ne 0) { throw ("NtQueryEaFile {0}: NTSTATUS 0x{1:X8}" -f $file, $rc) }
        # FILE_FULL_EA_INFORMATION: next entry offset, flags, name length,
        # value length, the name and its NUL, then the value.
        $at = 0
        while ($true) {
            $next = [BitConverter]::ToUInt32($buf, $at)
            $nameLen = $buf[$at + 5]
            $valueLen = [BitConverter]::ToUInt16($buf, $at + 6)
            $name = [System.Text.Encoding]::ASCII.GetString($buf, $at + 8, $nameLen)
            $value = New-Object byte[] $valueLen
            [Array]::Copy($buf, $at + 8 + $nameLen + 1, $value, 0, $valueLen)
            $eas[$name] = $value
            if ($next -eq 0) { break }
            $at += $next
        }
    } finally { $handle.Dispose() }
    return $eas
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
            $expected += "USER.E$i"
        }
        $eas = Read-WindowsEas $target
        $listed = @($eas.Keys)
        $missing = @($expected | Where-Object { $listed -notcontains $_ })
        $extra = @($listed | Where-Object { $expected -notcontains $_ })
        if ($missing.Count -gt 0 -or $extra.Count -gt 0) {
            throw "Windows lists $($listed.Count) EAs on $target, want $($expected.Count): missing [$($missing -join ', ')], unexpected [$($extra -join ', ')]"
        }
        foreach ($name in $expected) {
            $i = [int]$name.Substring(6)
            $unit = [System.Text.Encoding]::ASCII.GetBytes("ea $i ")
            $got = $eas[$name]
            if ($got.Length -ne $sizeInt) {
                throw "EA $name of $target is $($got.Length) bytes, want $sizeInt"
            }
            for ($b = 0; $b -lt $sizeInt; $b++) {
                if ($got[$b] -ne $unit[$b % $unit.Length]) {
                    throw "EA $name of $target differs at byte $b"
                }
            }
        }
        "Windows read back the $($expected.Count) EAs of $sizeInt bytes on $target" |
            Out-File "$Diag\verify-eas-result.txt" -Encoding UTF8
    } catch {
        $_.Exception.Message | Out-File "$Diag\verify-eas-error.txt" -Encoding UTF8
        throw
    }

    exit 0
} finally {
    Dismount-VhdAndCleanup -Vhd $Vhd -ImagePath $ImagePath -KeepImage $KeepImageBool
}
